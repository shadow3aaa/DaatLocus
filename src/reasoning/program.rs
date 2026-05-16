use schemars::JsonSchema;
use serde::{Serialize, de::DeserializeOwned};

use super::{examples::ProgramExample, optimizer::PromptTuningConfig, signature::Signature};

pub trait Program {
    type Output: DeserializeOwned + Serialize + JsonSchema + Clone;

    fn name(&self) -> &'static str;

    fn description(&self) -> &'static str;

    fn tuning_key(&self) -> String {
        self.name().to_string()
    }

    fn signature(&self) -> Signature;

    fn examples(&self) -> Vec<ProgramExample<Self::Output>> {
        Vec::new()
    }

    fn default_tuning(&self) -> PromptTuningConfig<Self::Output> {
        PromptTuningConfig {
            extra_instructions: Vec::new(),
            examples: self.examples(),
        }
    }

    fn include_history_messages(&self) -> bool {
        false
    }
}
