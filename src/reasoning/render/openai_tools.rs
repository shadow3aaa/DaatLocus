use schemars::schema_for;

use crate::context::Context;
use crate::reasoning::{
    examples::ProgramExample,
    ir::PromptIR,
    optimizer::PromptTuningConfig,
    program::Program,
    runtime::{PromptMessage, PromptRequest},
    signature::Signature,
};

use super::Renderer;

pub struct OpenAIToolRenderer;

impl Renderer for OpenAIToolRenderer {
    fn render<P: Program>(
        &self,
        context: &Context,
        program: &P,
        mut ir: PromptIR,
        tuning: &PromptTuningConfig<P::Output>,
    ) -> PromptRequest {
        let signature = program.signature();
        let examples = if tuning.examples.is_empty() {
            program.examples()
        } else {
            tuning.examples.clone()
        };

        for instruction in &tuning.extra_instructions {
            ir.instructions.push(instruction.clone());
        }

        let mut user_sections = Vec::new();
        user_sections.push(render_signature_block(&signature));
        if !examples.is_empty() {
            user_sections.push(render_examples_block(&examples));
        }
        if !ir.instructions.is_empty() {
            user_sections.push(format!("任务说明：\n{}", ir.instructions.join("\n")));
        }
        for section in ir.sections {
            user_sections.push(format!("## {}\n{}", section.title, section.body));
        }

        PromptRequest {
            tool_name: program.name().to_string(),
            tool_description: program.description().to_string(),
            output_schema: serde_json::to_value(schema_for!(P::Output)).unwrap(),
            system_messages: ir.system,
            long_term_memory_messages: if program.include_long_term_memory_messages() {
                build_long_term_memory_messages(context)
            } else {
                Vec::new()
            },
            history_messages: if program.include_history_messages() {
                context.memory.prompt_messages()
            } else {
                Vec::new()
            },
            current_user_message: user_sections.join("\n\n"),
            retry_messages: Vec::new(),
        }
    }
}

fn build_long_term_memory_messages(context: &Context) -> Vec<PromptMessage> {
    let mut messages = Vec::new();
    if !context.prompt_memory.recalled_memories.is_empty() {
        messages.push(PromptMessage::system(format!(
            "相关长期记忆：\n{}",
            context.prompt_memory.recalled_memories.join("\n")
        )));
    }
    if let Some(reflection) = &context.prompt_memory.reflected_strategy {
        messages.push(PromptMessage::system(format!(
            "相关长期反思：\n{reflection}"
        )));
    }
    messages
}

fn render_signature_block(signature: &Signature) -> String {
    let mut sections = vec![format!("程序目标：\n{}", signature.objective)];

    if !signature.inputs.is_empty() {
        sections.push(format!(
            "输入签名：\n{}",
            signature
                .inputs
                .iter()
                .map(|field| format!("- {}: {}", field.name, field.description))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }

    if !signature.outputs.is_empty() {
        sections.push(format!(
            "输出签名：\n{}",
            signature
                .outputs
                .iter()
                .map(|field| format!("- {}: {}", field.name, field.description))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }

    if !signature.rules.is_empty() {
        sections.push(format!("签名约束：\n{}", signature.rules.join("\n")));
    }

    format!("## 程序签名\n{}", sections.join("\n\n"))
}

fn render_examples_block<O: serde::Serialize>(examples: &[ProgramExample<O>]) -> String {
    let sections = examples
        .iter()
        .enumerate()
        .map(|(index, example)| {
            let mut body = vec![format!("### 示例 {}\n{}", index + 1, example.title)];
            if !example.inputs.is_empty() {
                body.push(format!(
                    "输入：\n{}",
                    example
                        .inputs
                        .iter()
                        .map(|field| format!("- {}: {}", field.name, field.value))
                        .collect::<Vec<_>>()
                        .join("\n")
                ));
            }
            body.push(format!(
                "输出(JSON)：\n```json\n{}\n```",
                serde_json::to_string_pretty(&example.output).unwrap()
            ));
            body.join("\n\n")
        })
        .collect::<Vec<_>>();

    format!("## 示例\n{}", sections.join("\n\n"))
}
