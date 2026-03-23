use serde::{Deserialize, Serialize};

use super::examples::ProgramExample;

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct PromptTuningConfig<O> {
    pub extra_instructions: Vec<String>,
    pub examples: Vec<ProgramExample<O>>,
}
