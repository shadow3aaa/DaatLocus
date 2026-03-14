use super::{ir::PromptIR, program::Program, runtime::PromptRequest};

pub mod openai_tools;

pub trait Renderer {
    fn render<P: Program>(&self, program: &P, ir: PromptIR) -> PromptRequest;
}
