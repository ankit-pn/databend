// Copyright 2022 Datafuse Labs.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use common_arrow::arrow::bitmap::Bitmap;
use common_arrow::arrow::bitmap::MutableBitmap;
use common_catalog::table_context::TableContext;
use common_exception::ErrorCode;
use common_exception::Result;
use common_expression::arrow::combine_validities_3;
use common_expression::types::nullable::NullableColumn;
use common_expression::types::nullable::NullableColumnBuilder;
use common_expression::types::AnyType;
use common_expression::types::DataType;
use common_expression::Chunk;
use common_expression::ChunkEntry;
use common_expression::Column;
use common_expression::Evaluator;
use common_expression::Expr;
use common_expression::Scalar;
use common_expression::Value;
use common_functions_v2::scalars::BUILTIN_FUNCTIONS;
use common_hashtable::HashtableLike;

use crate::pipelines::processors::transforms::hash_join::desc::MarkerKind;
use crate::pipelines::processors::transforms::hash_join::row::RowPtr;
use crate::pipelines::processors::JoinHashTable;
use crate::sql::plans::JoinType;

/// Some common methods for hash join.
impl JoinHashTable {
    // Merge build chunk and probe chunk that have the same number of rows
    pub(crate) fn merge_eq_chunk(&self, build_chunk: &Chunk, probe_chunk: &Chunk) -> Result<Chunk> {
        let mut probe_chunk = probe_chunk.clone();
        for col in build_chunk.columns() {
            probe_chunk.add_column(col.clone());
        }
        Ok(probe_chunk)
    }

    #[inline]
    pub(crate) fn probe_key<'a, H: HashtableLike<Value = Vec<RowPtr>>>(
        &self,
        hash_table: &'a H,
        key: &'a H::Key,
        valids: &Option<Bitmap>,
        i: usize,
    ) -> Option<H::EntryRef<'a>> {
        if valids.as_ref().map_or(true, |v| v.get_bit(i)) {
            return hash_table.entry(key);
        }
        None
    }

    pub(crate) fn create_marker_chunk(
        &self,
        has_null: bool,
        markers: Vec<MarkerKind>,
    ) -> Result<Chunk> {
        let mut validity = MutableBitmap::with_capacity(markers.len());
        let mut boolean_bit_map = MutableBitmap::with_capacity(markers.len());

        for m in markers {
            let marker = if m == MarkerKind::False && has_null {
                MarkerKind::Null
            } else {
                m
            };
            if marker == MarkerKind::Null {
                validity.push(false);
            } else {
                validity.push(true);
            }
            if marker == MarkerKind::True {
                boolean_bit_map.push(true);
            } else {
                boolean_bit_map.push(false);
            }
        }
        let num_rows = validity.len();
        let boolean_column = Column::Boolean(boolean_bit_map.into());
        let marker_column = Column::Nullable(Box::new(NullableColumn {
            column: boolean_column,
            validity: validity.into(),
        }));
        Ok(Chunk::new_from_sequence(
            vec![(
                Value::Column(marker_column),
                DataType::Nullable(Box::new(DataType::Boolean)),
            )],
            num_rows,
        ))
    }

    pub(crate) fn init_markers(cols: &[(Column, DataType)], num_rows: usize) -> Vec<MarkerKind> {
        let mut markers = vec![MarkerKind::False; num_rows];
        if cols
            .iter()
            .any(|(c, _)| matches!(c, Column::Null { .. } | Column::Nullable(_)))
        {
            let mut valids = None;
            for (col, _) in cols.iter() {
                match col {
                    Column::Nullable(c) => {
                        let bitmap = &c.validity;
                        if bitmap.unset_bits() == 0 {
                            let mut m = MutableBitmap::with_capacity(num_rows);
                            m.extend_constant(num_rows, true);
                            valids = Some(m.into());
                            break;
                        } else {
                            valids = combine_validities_3(valids, Some(bitmap.clone()));
                        }
                    }
                    Column::Null { .. } => {}
                    c => {
                        let mut m = MutableBitmap::with_capacity(num_rows);
                        m.extend_constant(num_rows, true);
                        valids = Some(m.into());
                        break;
                    }
                }
            }
            if let Some(v) = valids {
                for (idx, marker) in markers.iter_mut().enumerate() {
                    if !v.get_bit(idx) {
                        *marker = MarkerKind::Null;
                    }
                }
            }
        }
        markers
    }

    pub(crate) fn set_validity(column: &ChunkEntry, validity: &Bitmap) -> ChunkEntry {
        let (value, data_type) = (&column.value, &column.data_type);

        match value {
            Value::Scalar(s) => {
                let valid = validity.get_bit(0);
                if valid {
                    ChunkEntry {
                        id: column.id,
                        value: Value::Scalar(s.clone()),
                        data_type: data_type.wrap_nullable(),
                    }
                } else {
                    ChunkEntry {
                        id: column.id,
                        value: Value::Scalar(Scalar::Null),
                        data_type: data_type.wrap_nullable(),
                    }
                }
            }
            Value::Column(col) => {
                if matches!(col, Column::Null { .. }) {
                    column.clone()
                } else if let Some(col) = col.as_nullable() {
                    if col.len() == 0 {
                        return ChunkEntry {
                            id: column.id,
                            value: Value::Column(Column::Null {
                                len: validity.len(),
                            }),
                            data_type: data_type.clone(),
                        };
                    }
                    // It's possible validity is longer than col.
                    let diff_len = validity.len() - col.validity.len();
                    let mut new_validity = MutableBitmap::with_capacity(validity.len());
                    for (b1, b2) in validity.iter().zip(col.validity.iter()) {
                        new_validity.push(b1 & b2);
                    }
                    new_validity.extend_constant(diff_len, false);
                    let col = Column::Nullable(Box::new(NullableColumn {
                        column: col.column.clone(),
                        validity: new_validity.into(),
                    }));
                    ChunkEntry {
                        id: column.id,
                        value: Value::Column(col),
                        data_type: data_type.clone(),
                    }
                } else {
                    let col = Column::Nullable(Box::new(NullableColumn {
                        column: col.clone(),
                        validity: validity.clone(),
                    }));
                    ChunkEntry {
                        id: column.id,
                        value: Value::Column(col),
                        data_type: data_type.clone(),
                    }
                }
            }
        }
    }

    // return an (option bitmap, all_true, all_false)
    pub(crate) fn get_other_filters(
        &self,
        merged_chunk: &Chunk,
        filter: &Expr,
    ) -> Result<(Option<Bitmap>, bool, bool)> {
        let func_ctx = self.ctx.try_get_function_context()?;
        // `predicate_column` contains a column, which is a boolean column.
        let evaluator = Evaluator::new(merged_chunk, func_ctx, &BUILTIN_FUNCTIONS);
        let filter_vector: Value<AnyType> = evaluator
            .run(filter)
            .map_err(|(_, e)| ErrorCode::Internal(format!("Invalid expression: {}", e)))?;
        let predict_boolean_nonull = Chunk::<usize>::cast_to_nonull_boolean(&filter_vector)
            .ok_or_else(|| ErrorCode::Internal("Cannot get the boolean column"))?;

        match predict_boolean_nonull {
            Value::Scalar(v) => return Ok((None, v, !v)),
            Value::Column(s) => {
                let count_zeros = s.unset_bits();
                let all_false = s.len() == count_zeros;
                Ok((Some(s), count_zeros == 0, all_false))
            }
        }
    }

    pub(crate) fn get_nullable_filter_column(
        &self,
        merged_chunk: &Chunk,
        filter: &Expr,
    ) -> Result<Column> {
        let func_ctx = self.ctx.try_get_function_context()?;
        let evaluator = Evaluator::new(merged_chunk, func_ctx, &BUILTIN_FUNCTIONS);
        let filter_vector: Value<AnyType> = evaluator
            .run(filter)
            .map_err(|(_, e)| ErrorCode::Internal(format!("Invalid expression: {}", e)))?;
        let filter_vector =
            filter_vector.convert_to_full_column(filter.data_type(), merged_chunk.num_rows());

        match filter_vector {
            Column::Nullable(_) => Ok(filter_vector),
            other => {
                let mut validity = MutableBitmap::with_capacity(other.len());
                validity.extend_constant(other.len(), true);
                Ok(Column::Nullable(Box::new(NullableColumn {
                    column: other,
                    validity: validity.into(),
                })))
            }
        }
    }

    pub(crate) fn find_unmatched_build_indexes(
        &self,
        row_state: &[Vec<usize>],
    ) -> Result<Vec<RowPtr>> {
        // For right/full join, build side will appear at least once in the joined table
        // Find the unmatched rows in build side
        let mut unmatched_build_indexes = vec![];
        for (chunk_index, chunk) in self.row_space.chunks.read().unwrap().iter().enumerate() {
            for row_index in 0..chunk.num_rows() {
                if row_state[chunk_index][row_index] == 0 {
                    unmatched_build_indexes.push(RowPtr::new(chunk_index, row_index));
                }
            }
        }
        Ok(unmatched_build_indexes)
    }

    // For unmatched build index, the method will produce null probe chunk
    // Then merge null_probe_chunk with unmatched_build_chunk
    pub(crate) fn null_chunks_for_right_join(
        &self,
        unmatched_build_indexes: &Vec<RowPtr>,
    ) -> Result<Chunk> {
        let mut unmatched_build_chunk = self.row_space.gather(unmatched_build_indexes)?;
        let num_rows = unmatched_build_chunk.num_rows();
        if self.hash_join_desc.join_type == JoinType::Full {
            let nullable_unmatched_build_columns = unmatched_build_chunk
                .columns()
                .map(|c| {
                    let mut probe_validity = MutableBitmap::new();
                    probe_validity.extend_constant(num_rows, true);
                    let probe_validity: Bitmap = probe_validity.into();
                    Self::set_validity(c, &probe_validity)
                })
                .collect::<Vec<_>>();
            unmatched_build_chunk = Chunk::new(nullable_unmatched_build_columns, num_rows);
        };
        // Create null chunk for unmatched rows in probe side
        let null_probe_chunk = Chunk::new_from_sequence(
            self.probe_schema
                .fields()
                .iter()
                .map(|df| (Value::Scalar(Scalar::Null), df.data_type().clone().into()))
                .collect(),
            unmatched_build_indexes.len(),
        );
        self.merge_eq_chunk(&unmatched_build_chunk, &null_probe_chunk)
    }

    // Final row_state for right join
    // Record row in build side that is matched how many rows in probe side.
    pub(crate) fn row_state_for_right_join(&self) -> Result<Vec<Vec<usize>>> {
        let build_indexes = self.hash_join_desc.join_state.build_indexes.read();
        let chunks = self.row_space.chunks.read().unwrap();
        let mut row_state = Vec::with_capacity(chunks.len());
        for chunk in chunks.iter() {
            let mut rows = Vec::with_capacity(chunk.num_rows());
            for _row_index in 0..chunk.num_rows() {
                rows.push(0);
            }
            row_state.push(rows);
        }

        for row_ptr in build_indexes.iter() {
            if self.hash_join_desc.join_type == JoinType::Full
                && row_ptr.marker == Some(MarkerKind::False)
            {
                continue;
            }
            row_state[row_ptr.chunk_index][row_ptr.row_index] += 1;
        }
        Ok(row_state)
    }

    pub(crate) fn rest_chunk(&self) -> Result<Chunk> {
        let rest_probe_chunks = self.hash_join_desc.join_state.rest_probe_chunks.read();
        if rest_probe_chunks.is_empty() {
            return Ok(Chunk::empty());
        }
        let probe_chunk = Chunk::concat(&rest_probe_chunks)?;
        let rest_build_indexes = self.hash_join_desc.join_state.rest_build_indexes.read();
        let mut build_chunk = self.row_space.gather(&rest_build_indexes)?;
        // For left join, wrap nullable for build chunk
        if matches!(
            self.hash_join_desc.join_type,
            JoinType::Left | JoinType::Single | JoinType::Full
        ) {
            let validity = self.hash_join_desc.join_state.validity.read();
            let validity: Bitmap = (*validity).clone().into();
            let num_rows = validity.len();
            let nullable_columns = if self.row_space.data_chunks().is_empty() {
                build_chunk
                    .columns()
                    .map(|c| ChunkEntry {
                        id: c.id,
                        value: Value::Scalar(Scalar::Null),
                        data_type: c.data_type.clone(),
                    })
                    .collect::<Vec<_>>()
            } else {
                build_chunk
                    .columns()
                    .map(|c| Self::set_validity(c, &validity))
                    .collect::<Vec<_>>()
            };
            build_chunk = Chunk::new(nullable_columns, num_rows);
        }

        self.merge_eq_chunk(&build_chunk, &probe_chunk)
    }

    // Add `data_block` for build table to `row_space`
    pub(crate) fn add_build_block(&self, input: Chunk) -> Result<()> {
        let func_ctx = self.ctx.try_get_function_context()?;
        let evaluator = Evaluator::new(&input, func_ctx, &BUILTIN_FUNCTIONS);

        let build_cols = self
            .hash_join_desc
            .build_keys
            .iter()
            .map(|expr| {
                let return_type = expr.data_type();
                Ok((
                    evaluator
                        .run(expr)
                        .map_err(|(_, e)| {
                            ErrorCode::Internal(format!("Invalid expression: {}", e))
                        })?
                        .convert_to_full_column(return_type, input.num_rows()),
                    return_type.clone(),
                ))
            })
            .collect::<Result<_>>()?;

        self.row_space.push_cols(input, build_cols)
    }
}
