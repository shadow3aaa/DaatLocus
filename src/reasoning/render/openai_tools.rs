use schemars::schema_for;

use crate::context::Context;
use crate::reasoning::{
    examples::ProgramExample,
    ir::PromptIR,
    optimizer::PromptTuningConfig,
    program::Program,
    prompt_doc::{PromptBlock, PromptDocument, PromptNode, PromptStateDoc},
    prompt_renderer::LlmPromptRenderer,
    prompt_text::{PromptTextBuilder, render_bullet_list},
    runtime::{HistoryMessage, PromptRequest},
    signature::Signature,
};
use crate::schema_utils::normalize_openai_json_schema;

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
        user_message.push_markdown_section("程序签名", render_signature_block(&signature));
        if !examples.is_empty() {
            user_message.push_markdown_section("示例", render_examples_block(&examples));
        }
        if !ir.instructions.is_empty() {
            user_message.push_labeled_section("任务说明", render_bullet_list(ir.instructions));
        }
        for section in ir.sections {
            user_message.push_markdown_section(section.title, section.body);
        }

        PromptRequest {
            tool_name: program.name().to_string(),
            tool_description: program.description().to_string(),
            output_schema: normalize_openai_json_schema(
                serde_json::to_value(schema_for!(P::Output)).unwrap(),
            ),
            system_messages: ir.system,
            long_term_memory_messages: if program.include_long_term_memory_messages() {
                build_long_term_memory_messages(context)
            } else {
                Vec::new()
            },
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

fn build_long_term_memory_messages(context: &Context) -> Vec<HistoryMessage> {
    if context.prompt_memory.is_empty() {
        return Vec::new();
    }

    let mut children = Vec::new();
    if !context.prompt_memory.observations.is_empty() {
        children.push(PromptNode::State(PromptStateDoc::new(
            "observations",
            context
                .prompt_memory
                .observations
                .iter()
                .map(|fact| {
                    PromptBlock::Paragraph(format!(
                        "id: {}\ntype: {}\ntext: {}",
                        fact.id,
                        fact.memory_type
                            .clone()
                            .unwrap_or_else(|| "observation".to_string()),
                        fact.text.trim()
                    ))
                })
                .collect(),
        )));
    }
    if !context.prompt_memory.raw_memories.is_empty() {
        children.push(PromptNode::State(PromptStateDoc::new(
            "raw_memories",
            context
                .prompt_memory
                .raw_memories
                .iter()
                .map(|fact| {
                    PromptBlock::Paragraph(format!(
                        "id: {}\ntype: {}\ntext: {}",
                        fact.id,
                        fact.memory_type
                            .clone()
                            .unwrap_or_else(|| "memory".to_string()),
                        fact.text.trim()
                    ))
                })
                .collect(),
        )));
    }
    if !context.prompt_memory.citations.is_empty() {
        children.push(PromptNode::State(PromptStateDoc::new(
            "citations",
            vec![PromptBlock::BulletList(
                context
                    .prompt_memory
                    .citations
                    .iter()
                    .map(|citation| {
                        format!("[{}] {}: {}", citation.kind, citation.id, citation.summary)
                    })
                    .collect(),
            )],
        )));
    }

    let doc = PromptDocument::new(vec![PromptNode::Group(
        super::super::prompt_doc::PromptGroupDoc::new("recall_memories", children),
    )]);

    LlmPromptRenderer::render_system_messages(&doc)
        .into_iter()
        .map(HistoryMessage::system)
        .collect()
}

fn render_signature_block(signature: &Signature) -> String {
    let mut builder = PromptTextBuilder::new();
    builder.push_labeled_section("程序目标", signature.objective.clone());
    if !signature.inputs.is_empty() {
        builder.push_bullet_list_section(
            "输入签名",
            signature
                .inputs
                .iter()
                .map(|field| format!("{}: {}", field.name, field.description)),
        );
    }
    if !signature.outputs.is_empty() {
        builder.push_bullet_list_section(
            "输出签名",
            signature
                .outputs
                .iter()
                .map(|field| format!("{}: {}", field.name, field.description)),
        );
    }
    if !signature.rules.is_empty() {
        builder.push_bullet_list_section("签名约束", signature.rules.clone());
    }
    builder.build()
}

fn render_examples_block<O: serde::Serialize>(examples: &[ProgramExample<O>]) -> String {
    let sections = examples
        .iter()
        .enumerate()
        .map(|(index, example)| {
            let mut body = PromptTextBuilder::new();
            body.push_paragraph(format!("### 示例 {}\n{}", index + 1, example.title));
            if !example.inputs.is_empty() {
                body.push_bullet_list_section(
                    "输入",
                    example
                        .inputs
                        .iter()
                        .map(|field| format!("{}: {}", field.name, field.value)),
                );
            }
            body.push_labeled_section(
                "输出(JSON)",
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
