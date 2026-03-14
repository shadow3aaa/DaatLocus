use schemars::JsonSchema;
use serde::{Serialize, de::DeserializeOwned};

use crate::{context::Context, snapshot::Snapshot};

use super::{ir::PromptIR, signature::Signature};

pub trait Program {
    type Output: DeserializeOwned + Serialize + JsonSchema;

    fn name(&self) -> &'static str;

    fn description(&self) -> &'static str;

    fn signature(&self) -> Signature;

    fn build_ir(&self, context: &Context, snapshot: &Snapshot) -> PromptIR;
}
