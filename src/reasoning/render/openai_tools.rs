use schemars::schema_for;

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
        program: &P,
        mut ir: PromptIR,
        tuning: &PromptTuningConfig<P::Output>,
    ) -> PromptRequest {
        let mut messages = Vec::new();
        let signature = program.signature();
        let examples = if tuning.examples.is_empty() {
            program.examples()
        } else {
            tuning.examples.clone()
        };

        for instruction in &tuning.extra_instructions {
            ir.instructions.push(instruction.clone());
        }

        if !ir.system.is_empty() {
            messages.push(PromptMessage::system(ir.system.join("\n\n")));
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
