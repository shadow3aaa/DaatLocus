use miette::{Result, miette};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    context::Context,
    core::LLM,
    logging::{RuntimeStatusLevel, set_runtime_status},
    snapshot::Snapshot,
    tool_ui::{ToolCallUiEvent, ToolUiEvent},
};

use super::{
    compiled::seed_compiled_program_from_tuning_for_model,
    optimizer::PromptTuningConfig,
    program::Program,
    render::Renderer,
    trace::{ProgramTraceRecord, TraceOrigin, append_program_trace},
};

pub struct ProgramExecutionOutcome<O> {
    pub output: O,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct PromptRequest {
    pub tool_name: String,
    pub tool_description: String,
    pub output_schema: Value,
    pub system_messages: Vec<String>,
    pub long_term_memory_messages: Vec<PromptMessage>,
    pub history_messages: Vec<PromptMessage>,
    pub current_user_message: String,
    #[serde(default)]
    pub retry_messages: Vec<PromptMessage>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct AgentToolSpec {
    pub name: String,
    pub description: String,
    pub input_spec: AgentToolInputSpec,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentToolInputSpec {
    JsonSchema {
        schema: Value,
    },
    FreeformGrammar {
        syntax: String,
        definition: String,
        fallback_schema: Value,
    },
}

#[derive(Clone, Serialize, Deserialize)]
pub struct AgentToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum AgentMessage {
    System {
        content: String,
    },
    User {
        content: String,
    },
    Assistant {
        content: String,
    },
    AssistantToolCallProtocol {
        content: Option<String>,
        calls: Vec<AgentToolCall>,
    },
    Tool {
        tool_call_id: String,
        name: String,
        content: String,
    },
}

#[derive(Clone, Serialize, Deserialize)]
pub struct AgentTurnRequest {
    pub messages: Vec<AgentMessage>,
    pub tools: Vec<AgentToolSpec>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum AgentTurnItem {
    AssistantMessage { content: String },
    ToolCall { call: AgentToolCall },
}

#[derive(Clone, Serialize, Deserialize)]
pub struct AgentTurnStreamResult {
    pub items: Vec<AgentTurnItem>,
    #[serde(alias = "needs_follow_up")]
    pub raw_stream_follow_up: bool,
    pub last_assistant_message: Option<String>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct PromptMemoryContext {
    pub recalled_memories: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct PromptMessage {
    pub role: PromptRole,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_ui_event: Option<ToolUiEvent>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_call_ui_events: Vec<ToolCallUiEvent>,
}

#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum PromptRole {
    System,
    User,
    Assistant,
    Tool,
}

impl PromptMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: PromptRole::System,
            content: content.into(),
            tool_ui_event: None,
            tool_call_ui_events: Vec::new(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: PromptRole::User,
            content: content.into(),
            tool_ui_event: None,
            tool_call_ui_events: Vec::new(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: PromptRole::Assistant,
            content: content.into(),
            tool_ui_event: None,
            tool_call_ui_events: Vec::new(),
        }
    }

    pub fn tool_with_ui(content: impl Into<String>, tool_ui_event: ToolUiEvent) -> Self {
        Self {
            role: PromptRole::Tool,
            content: content.into(),
            tool_ui_event: Some(tool_ui_event),
            tool_call_ui_events: Vec::new(),
        }
    }

    pub fn assistant_with_tool_calls(
        content: impl Into<String>,
        tool_call_ui_events: Vec<ToolCallUiEvent>,
    ) -> Self {
        Self {
            role: PromptRole::Assistant,
            content: content.into(),
            tool_ui_event: None,
            tool_call_ui_events,
        }
    }
}

impl PromptRequest {
    fn with_retry_note(&self, note: impl Into<String>) -> Self {
        let mut request = self.clone();
        request.retry_messages.push(PromptMessage::user(format!(
            "上一次输出未通过类型校验，请只修正输出结构并重试。\n\
错误：{}\n\
严格要求：\n\
1. 必须返回与 schema 完全匹配的单个 JSON 对象。\n\
2. 不要返回 markdown，不要使用 ```json 代码块，不要附加解释文字。\n\
3. 所有字段都必须提供；如果某字段当前不需要，请使用空字符串、false、0 或空数组，而不是 null，也不要省略。\n\
4. 枚举值必须逐字匹配 schema，不能自行改写名称。\n\
5. 如果 provider 支持 tool call，请把该 JSON 放在 tool arguments 中，而不是普通文本 content 里。",
            note.into()
        )));
        request
    }

    pub fn all_messages(&self) -> Vec<PromptMessage> {
        let mut messages = self
            .system_messages
            .iter()
            .cloned()
            .map(PromptMessage::system)
            .collect::<Vec<_>>();
        messages.extend(self.long_term_memory_messages.clone());
        messages.extend(self.history_messages.clone());
        messages.push(PromptMessage::user(self.current_user_message.clone()));
        messages.extend(self.retry_messages.clone());
        messages
    }
}

impl AgentMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self::System {
            content: content.into(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self::User {
            content: content.into(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self::Assistant {
            content: content.into(),
        }
    }

    pub fn assistant_tool_call_protocol(
        content: Option<String>,
        calls: Vec<AgentToolCall>,
    ) -> Self {
        Self::AssistantToolCallProtocol { content, calls }
    }

    pub fn tool(
        tool_call_id: impl Into<String>,
        name: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        Self::Tool {
            tool_call_id: tool_call_id.into(),
            name: name.into(),
            content: content.into(),
        }
    }
}

pub fn summarize_assistant_tool_call_protocol(
    content: Option<&str>,
    calls: &[AgentToolCall],
) -> String {
    let tool_names = calls
        .iter()
        .map(|call| call.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let note = content
        .map(summarize_agent_inline_text)
        .filter(|text| !text.is_empty());
    match note {
        Some(note) => format!("assistant tool-call protocol [{tool_names}] with note: {note}"),
        None => format!("assistant tool-call protocol [{tool_names}]"),
    }
}

pub fn assistant_tool_call_protocol_char_count(
    content: Option<&str>,
    calls: &[AgentToolCall],
) -> usize {
    content.unwrap_or_default().chars().count()
        + calls
            .iter()
            .map(|call| {
                call.name.chars().count()
                    + call.id.chars().count()
                    + call.arguments.to_string().chars().count()
            })
            .sum::<usize>()
}

pub fn estimate_assistant_tool_call_protocol_tokens(
    calls: &[AgentToolCall],
    estimate_json_value_tokens: impl Fn(&Value) -> usize,
    approx_token_count: impl Fn(&str) -> usize,
) -> usize {
    calls
        .iter()
        .map(|call| {
            approx_token_count(&call.id)
                .saturating_add(approx_token_count(&call.name))
                .saturating_add(estimate_json_value_tokens(&call.arguments))
                .saturating_add(16)
        })
        .sum()
}

pub fn render_assistant_tool_call_protocol_dump(
    content: Option<&str>,
    calls: &[AgentToolCall],
) -> Vec<String> {
    let mut lines = vec!["role=assistant".to_string()];
    if let Some(content) = content
        && !content.trim().is_empty()
    {
        lines.push("content:".to_string());
        lines.push(content.to_string());
    }
    lines.push(format!("tool_call_count={}", calls.len()));
    for (index, call) in calls.iter().enumerate() {
        lines.push(format!("tool_call[{}].id={}", index, call.id));
        lines.push(format!("tool_call[{}].name={}", index, call.name));
        lines.push(format!("tool_call[{}].arguments=", index));
        lines.push(
            serde_json::to_string_pretty(&call.arguments)
                .unwrap_or_else(|_| call.arguments.to_string()),
        );
    }
    lines
}

fn summarize_agent_inline_text(text: &str) -> String {
    const MAX_CHARS: usize = 120;
    let compact = text.replace('\n', "\\n");
    let mut chars = compact.chars();
    let summary = chars.by_ref().take(MAX_CHARS).collect::<String>();
    if chars.next().is_some() {
        format!("{summary}...")
    } else {
        summary
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
    let tuning = resolve_program_tuning(context, program).await;
    execute_program_with_ir(llm, context, renderer, program, ir, &tuning).await
}

pub async fn resolve_program_tuning<P: Program>(
    context: &Context,
    program: &P,
) -> PromptTuningConfig<P::Output> {
    if let Some(tuning) = context.compiled_prompts.get_tuning(program) {
        return tuning;
    }

    let tuning = program.default_tuning();
    if let Err(err) = seed_compiled_program_from_tuning_for_model(
        &context.config.main_model.model_name,
        program,
        &tuning,
    )
    .await
    {
        log_prompt_compile_event(
            context,
            format!(
                "[prompt-compile] failed to seed compiled tuning for {}: {err:?}",
                program.tuning_key()
            ),
        );
    } else {
        log_prompt_compile_event(
            context,
            format!(
                "[prompt-compile] seeded compiled tuning for {}",
                program.tuning_key()
            ),
        );
    }
    tuning
}

fn log_prompt_compile_event(context: &Context, message: String) {
    set_runtime_status(
        context.dashboard_tx.as_ref(),
        RuntimeStatusLevel::Info,
        message,
    );
}

pub async fn execute_program_with_ir<P: Program, R: Renderer>(
    llm: &(dyn LLM + Send + Sync),
    context: &Context,
    renderer: &R,
    program: &P,
    ir: super::ir::PromptIR,
    tuning: &PromptTuningConfig<P::Output>,
) -> Result<P::Output> {
    execute_program_with_ir_report(
        llm,
        context,
        renderer,
        program,
        ir,
        tuning,
        TraceOrigin::Runtime,
    )
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
    trace_origin: TraceOrigin,
) -> Result<ProgramExecutionOutcome<P::Output>> {
    let mut request = renderer.render(context, program, ir, tuning);
    let mut last_error = None;
    let signature = program.signature();

    for attempt in 0..2 {
        let value = match llm.run_json(context, request.clone()).await {
            Ok(value) => value,
            Err(err) => {
                let error_text = err.to_string();
                append_program_trace(ProgramTraceRecord::new(
                    trace_origin,
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
                    trace_origin,
                    program.name(),
                    attempt + 1,
                    signature.clone(),
                    request.clone(),
                    value,
                    serde_json::to_value(&output).ok(),
                    None,
                ))
                .await;
                return Ok(ProgramExecutionOutcome { output });
            }
            Err(err) => {
                last_error = Some(err.to_string());
                append_program_trace(ProgramTraceRecord::new(
                    trace_origin,
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
