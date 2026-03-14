use miette::{Result, miette};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{context::Context, core::LLM, snapshot::Snapshot};

use super::{
    optimizer::PromptTuningConfig,
    program::Program,
    render::Renderer,
    trace::{ProgramTraceRecord, append_program_trace},
};

pub struct ProgramExecutionOutcome<O> {
    pub output: O,
    pub attempts_used: usize,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct PromptRequest {
    pub tool_name: String,
    pub tool_description: String,
    pub output_schema: Value,
    pub messages: Vec<PromptMessage>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct PromptMessage {
    pub role: PromptRole,
    pub content: String,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
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
    let tuning = context
        .compiled_prompts
        .get_tuning(program)
        .unwrap_or_else(|| program.default_tuning());
    execute_program_with_ir(llm, context, renderer, program, ir, &tuning).await
}

pub async fn execute_program_with_ir<P: Program, R: Renderer>(
    llm: &(dyn LLM + Send + Sync),
    context: &Context,
    renderer: &R,
    program: &P,
    ir: super::ir::PromptIR,
    tuning: &PromptTuningConfig<P::Output>,
) -> Result<P::Output> {
    execute_program_with_ir_report(llm, context, renderer, program, ir, tuning)
        .await
        .map(|outcome| outcome.output)
}

pub async fn execute_program_with_ir_report<P: Program, R: Renderer>(
    llm: &(dyn LLM + Send + Sync),
    context: &Context,
    renderer: &R,
    program: &P,
    ir: super::ir::PromptIR,
    tuning: &PromptTuningConfig<P::Output>,
) -> Result<ProgramExecutionOutcome<P::Output>> {
    let mut request = renderer.render(program, ir, tuning);
    let mut last_error = None;
    let signature = program.signature();

    for attempt in 0..2 {
        let value = match llm.run_json(context, request.clone()).await {
            Ok(value) => value,
            Err(err) => {
                let error_text = err.to_string();
                append_program_trace(ProgramTraceRecord::new(
                    program.name(),
                    attempt + 1,
                    signature.clone(),
                    request.clone(),
                    json!({ "provider_error": error_text }),
                    None,
                    Some(err.to_string()),
                ))
                .await;
                last_error = Some(error_text.clone());
                request = request.with_retry_note(error_text);
                continue;
            }
        };
        match serde_json::from_value::<P::Output>(value.clone()) {
            Ok(output) => {
                append_program_trace(ProgramTraceRecord::new(
                    program.name(),
                    attempt + 1,
                    signature.clone(),
                    request.clone(),
                    value,
                    serde_json::to_value(&output).ok(),
                    None,
                ))
                .await;
                return Ok(ProgramExecutionOutcome {
                    output,
                    attempts_used: attempt + 1,
                });
            }
            Err(err) => {
                last_error = Some(err.to_string());
                append_program_trace(ProgramTraceRecord::new(
                    program.name(),
                    attempt + 1,
                    signature.clone(),
                    request.clone(),
                    value,
                    None,
                    Some(err.to_string()),
                ))
                .await;
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
