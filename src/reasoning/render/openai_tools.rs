use crate::context::Context;
use crate::reasoning::{
    examples::ProgramExample,
    ir::PromptIR,
    optimizer::PromptTuningConfig,
    program::Program,
    prompt_text::{PromptTextBuilder, render_bullet_list},
    runtime::PromptRequest,
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

        let mut user_message = PromptTextBuilder::new();
        user_message.push_markdown_section("Program Signature", render_signature_block(&signature));
        if !examples.is_empty() {
            user_message.push_markdown_section("Examples", render_examples_block(&examples));
        }
        if !ir.instructions.is_empty() {
            user_message
                .push_labeled_section("Task Instructions", render_bullet_list(ir.instructions));
        }
        for section in ir.sections {
            user_message.push_markdown_section(section.title, section.body);
        }

        PromptRequest {
            tool_name: program.name().to_string(),
            tool_description: program.description().to_string(),
            output_schema: program.output_schema(),
            system_messages: ir.system,
            long_term_memory_messages: Vec::new(),
            history_messages: if program.include_history_messages() {
                context.memory.runtime_conversation_messages()
            } else {
                Vec::new()
            },
            current_user_message: user_message.build(),
            retry_messages: Vec::new(),
        }
    }
}

fn render_signature_block(signature: &Signature) -> String {
    let mut builder = PromptTextBuilder::new();
    builder.push_labeled_section("Objective", signature.objective.clone());
    if !signature.inputs.is_empty() {
        builder.push_bullet_list_section(
            "Input Signature",
            signature
                .inputs
                .iter()
                .map(|field| format!("{}: {}", field.name, field.description)),
        );
    }
    if !signature.outputs.is_empty() {
        builder.push_bullet_list_section(
            "Output Signature",
            signature
                .outputs
                .iter()
                .map(|field| format!("{}: {}", field.name, field.description)),
        );
    }
    if !signature.rules.is_empty() {
        builder.push_bullet_list_section("Signature Rules", signature.rules.clone());
    }
    builder.build()
}

fn render_examples_block<O: serde::Serialize>(examples: &[ProgramExample<O>]) -> String {
    let sections = examples
        .iter()
        .enumerate()
        .map(|(index, example)| {
            let mut body = PromptTextBuilder::new();
            body.push_paragraph(format!("### Example {}\n{}", index + 1, example.title));
            if !example.inputs.is_empty() {
                body.push_bullet_list_section(
                    "Inputs",
                    example
                        .inputs
                        .iter()
                        .map(|field| format!("{}: {}", field.name, field.value)),
                );
            }
            body.push_labeled_section(
                "Output (JSON)",
                format!(
                    "```json\n{}\n```",
                    serde_json::to_string_pretty(&example.output).unwrap()
                ),
            );
            body.build()
        })
        .collect::<Vec<_>>();

    sections.join("\n\n")
}
