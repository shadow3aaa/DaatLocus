use schemars::JsonSchema;
use serde::{Serialize, de::DeserializeOwned};

use crate::schema_utils::{ModelSchema, model_schema_for};

use super::{optimizer::PromptTuningConfig, signature::Signature};

pub trait Program: Sync {
    type Output: DeserializeOwned + Serialize + JsonSchema + ModelSchema + Clone + Send + Sync;

    fn name(&self) -> &'static str;

    fn description(&self) -> &'static str;

    fn output_schema(&self) -> serde_json::Value {
        model_schema_for::<Self::Output>()
    }

    fn tuning_key(&self) -> String {
        self.name().to_string()
    }

    fn signature(&self) -> Signature;

    fn default_tuning(&self) -> PromptTuningConfig<Self::Output> {
        PromptTuningConfig {
            extra_instructions: Vec::new(),
            examples: Vec::new(),
        }
    }
}
