use miette::{Result, miette};
use serde_json::Value;

use crate::{context::Context, core::LLM, snapshot::Snapshot};

use super::{program::Program, render::Renderer};

#[derive(Clone)]
pub struct PromptRequest {
    pub tool_name: String,
    pub tool_description: String,
    pub output_schema: Value,
    pub messages: Vec<PromptMessage>,
}

#[derive(Clone)]
pub struct PromptMessage {
    pub role: PromptRole,
    pub content: String,
}

#[derive(Clone, Copy)]
pub enum PromptRole {
    System,
    User,
}

impl PromptMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: PromptRole::System,
            content: content.into(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: PromptRole::User,
            content: content.into(),
        }
    }
}

impl PromptRequest {
    fn with_retry_note(&self, note: impl Into<String>) -> Self {
        let mut request = self.clone();
        request.messages.push(PromptMessage::user(format!(
            "上一次输出未通过类型校验，请只修正输出结构并重试。\n错误：{}",
            note.into()
        )));
        request
    }
}

pub async fn execute_program<P: Program, R: Renderer>(
    llm: &(dyn LLM + Send + Sync),
    context: &Context,
    snapshot: &Snapshot,
    renderer: &R,
    program: &P,
) -> Result<P::Output> {
    let ir = program.build_ir(context, snapshot);
    let mut request = renderer.render(program, ir);
    let mut last_error = None;

    for _ in 0..2 {
        let value = llm.run_json(context, request.clone()).await;
        match serde_json::from_value::<P::Output>(value) {
            Ok(output) => return Ok(output),
            Err(err) => {
                last_error = Some(err.to_string());
                request = request.with_retry_note(err.to_string());
            }
        }
    }

    Err(miette!(
        "program {} failed to deserialize output: {}",
        program.name(),
        last_error.unwrap_or_else(|| "unknown error".to_string())
    ))
}
