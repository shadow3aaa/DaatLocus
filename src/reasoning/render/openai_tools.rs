use schemars::schema_for;

use crate::reasoning::{
    ir::PromptIR,
    program::Program,
    runtime::{PromptMessage, PromptRequest},
};

use super::Renderer;

pub struct OpenAIToolRenderer;

impl Renderer for OpenAIToolRenderer {
    fn render<P: Program>(&self, program: &P, ir: PromptIR) -> PromptRequest {
        let mut messages = Vec::new();

        if !ir.system.is_empty() {
            messages.push(PromptMessage::system(ir.system.join("\n\n")));
        }

        let mut user_sections = Vec::new();
        if !ir.instructions.is_empty() {
            user_sections.push(format!("任务说明：\n{}", ir.instructions.join("\n")));
        }
        for section in ir.sections {
            user_sections.push(format!("## {}\n{}", section.title, section.body));
        }
        if !user_sections.is_empty() {
            messages.push(PromptMessage::user(user_sections.join("\n\n")));
        }

        PromptRequest {
            tool_name: program.name().to_string(),
            tool_description: program.description().to_string(),
            output_schema: serde_json::to_value(schema_for!(P::Output)).unwrap(),
            messages,
        }
    }
}
