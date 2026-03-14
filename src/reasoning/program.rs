use schemars::JsonSchema;
use serde::{Serialize, de::DeserializeOwned};

use crate::{context::Context, snapshot::Snapshot};

use super::ir::PromptIR;

pub trait Program {
    type Output: DeserializeOwned + Serialize + JsonSchema;

    fn name(&self) -> &'static str;

    fn description(&self) -> &'static str;

    fn build_ir(&self, context: &Context, snapshot: &Snapshot) -> PromptIR;
}
