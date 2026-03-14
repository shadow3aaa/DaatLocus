use serde::{Deserialize, Serialize};

use super::examples::ProgramExample;

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct PromptTuningConfig<O> {
    pub extra_instructions: Vec<String>,
    pub examples: Vec<ProgramExample<O>>,
}

#[derive(Clone)]
pub struct CandidateConfig<O> {
    pub name: String,
    pub config: PromptTuningConfig<O>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct OptimizationResult {
    pub suite: String,
    pub best_candidate: String,
    pub score: usize,
    pub total_cases: usize,
}
