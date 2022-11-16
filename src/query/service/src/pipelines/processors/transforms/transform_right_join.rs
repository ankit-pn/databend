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

use std::sync::Arc;

use common_exception::Result;
use common_expression::Chunk;

use crate::pipelines::processors::transforms::Compactor;
use crate::pipelines::processors::HashJoinState;
use crate::pipelines::processors::TransformCompact;

pub struct RightJoinCompactor {
    hash_join_state: Arc<dyn HashJoinState>,
}

impl RightJoinCompactor {
    pub fn create(hash_join_state: Arc<dyn HashJoinState>) -> Self {
        RightJoinCompactor { hash_join_state }
    }
}

impl Compactor for RightJoinCompactor {
    fn name() -> &'static str {
        "RightJoinCompactor"
    }

    fn interrupt(&self) {
        self.hash_join_state.interrupt();
    }

    // `compact_final` is called when all the chunks are pushed
    fn compact_final(&self, chunks: &[Chunk]) -> Result<Vec<Chunk>> {
        self.hash_join_state.right_join_chunks(chunks)
    }
}

pub type TransformRightJoin = TransformCompact<RightJoinCompactor>;
