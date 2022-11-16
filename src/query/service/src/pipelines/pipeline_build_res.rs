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

use common_exception::Result;
use common_expression::Chunk;
use common_pipeline_core::processors::port::OutputPort;
use common_pipeline_core::Pipeline;
use common_pipeline_core::SourcePipeBuilder;
use common_pipeline_sources::processors::sources::OneChunkSource;

pub struct PipelineBuildResult {
    pub main_pipeline: Pipeline,
    // Containing some sub queries pipelines, must be complete pipeline
    pub sources_pipelines: Vec<Pipeline>,
}

impl PipelineBuildResult {
    pub fn create() -> PipelineBuildResult {
        PipelineBuildResult {
            main_pipeline: Pipeline::create(),
            sources_pipelines: vec![],
        }
    }

    pub fn from_chunks(chunks: Vec<Chunk>) -> Result<PipelineBuildResult> {
        let mut source_builder = SourcePipeBuilder::create();

        for chunk in chunks {
            let output = OutputPort::create();
            source_builder.add_source(output.clone(), OneChunkSource::create(output, chunk)?);
        }

        let mut main_pipeline = Pipeline::create();
        main_pipeline.add_pipe(source_builder.finalize());

        Ok(PipelineBuildResult {
            main_pipeline,
            sources_pipelines: vec![],
        })
    }

    pub fn set_max_threads(&mut self, max_threads: usize) {
        self.main_pipeline.set_max_threads(max_threads);

        for source_pipeline in &mut self.sources_pipelines {
            source_pipeline.set_max_threads(max_threads);
        }
    }
}
