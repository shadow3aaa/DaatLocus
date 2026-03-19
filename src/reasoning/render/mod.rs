use crate::context::Context;

use super::{
    ir::PromptIR, optimizer::PromptTuningConfig, program::Program, runtime::PromptRequest,
};

pub mod openai_tools;

pub trait Renderer {
    fn render<P: Program>(
        &self,
        context: &Context,
        program: &P,
        ir: PromptIR,
        tuning: &PromptTuningConfig<P::Output>,
    ) -> PromptRequest;
}
