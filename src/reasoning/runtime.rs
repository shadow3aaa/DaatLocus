use crate::{
    context::Context,
    core::Llm,
    logging::{RuntimeStatusLevel, set_runtime_status},
    tool_ui::{ToolCallUiEvent, ToolUiEvent},
};
use miette::{Result, miette};
use serde::{
    Deserialize, Deserializer, Serialize, Serializer, de::DeserializeOwned, ser::SerializeStruct,
};
use serde_json::{Value, json};

use super::{
    compiled::seed_compiled_program_from_tuning_for_model,
    optimizer::PromptTuningConfig,
    program::Program,
    render::Renderer,
    signature::Signature,
    trace::{ProgramTraceRecord, ProgramTraceRecordParts, TraceOrigin, append_program_trace},
};

pub struct ProgramExecutionOutcome<O> {
    pub output: O,
}

struct ProgramExecutionRequest<'a, P: Program, R: Renderer> {
    llm: &'a (dyn Llm + Send + Sync),
    context: &'a Context,
    renderer: &'a R,
    program: &'a P,
    ir: super::ir::PromptIR,
    tuning: &'a PromptTuningConfig<P::Output>,
    trace_origin: TraceOrigin,
    max_retry_count: usize,
}

struct PromptRequestExecution<'a> {
    llm: &'a (dyn Llm + Send + Sync),
    context: &'a Context,
    program_name: &'a str,
    signature: Signature,
    request: PromptRequest,
    trace_origin: TraceOrigin,
    max_retry_count: usize,
}

const DEFAULT_PROGRAM_RETRY_COUNT: usize = 1;

#[derive(Clone, Serialize, Deserialize)]
pub struct PromptRequest {
    pub tool_name: String,
    pub tool_description: String,
    pub output_schema: Value,
    pub system_messages: Vec<String>,
    pub long_term_memory_messages: Vec<HistoryMessage>,
    pub history_messages: Vec<HistoryMessage>,
    pub current_user_message: String,
    #[serde(default)]
    pub retry_messages: Vec<HistoryMessage>,
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

#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AgentContent {
    text: String,
    parts: Vec<AgentContentPart>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentContentPart {
    Text {
        text: String,
    },
    Image {
        path: String,
        media_type: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
}

#[derive(Deserialize)]
#[serde(untagged)]
enum AgentContentWire {
    Text(String),
    Rich {
        #[serde(default)]
        text: String,
        #[serde(default)]
        parts: Vec<AgentContentPart>,
    },
}

impl AgentContent {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            parts: Vec::new(),
        }
    }

    pub fn multimodal(text: impl Into<String>, parts: Vec<AgentContentPart>) -> Self {
        Self {
            text: text.into(),
            parts,
        }
    }

    pub fn as_text(&self) -> &str {
        &self.text
    }

    pub fn parts(&self) -> &[AgentContentPart] {
        &self.parts
    }

    pub fn is_plain_text(&self) -> bool {
        self.parts.is_empty()
    }

    pub fn with_text(mut self, text: impl Into<String>) -> Self {
        self.text = text.into();
        self
    }
}

impl From<String> for AgentContent {
    fn from(value: String) -> Self {
        Self::text(value)
    }
}

impl From<&str> for AgentContent {
    fn from(value: &str) -> Self {
        Self::text(value)
    }
}

impl Serialize for AgentContent {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if self.parts.is_empty() {
            return serializer.serialize_str(&self.text);
        }
        let mut state = serializer.serialize_struct("AgentContent", 2)?;
        state.serialize_field("text", &self.text)?;
        state.serialize_field("parts", &self.parts)?;
        state.end()
    }
}

impl<'de> Deserialize<'de> for AgentContent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        match AgentContentWire::deserialize(deserializer)? {
            AgentContentWire::Text(text) => Ok(Self::text(text)),
            AgentContentWire::Rich { text, parts } => Ok(Self::multimodal(text, parts)),
        }
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind")]
pub enum AgentMessage {
    System {
        content: String,
    },
    User {
        content: AgentContent,
    },
    Assistant {
        content: String,
    },
    AssistantToolCallProtocol {
        content: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning_content: Option<String>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_reasoning_content: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct HistoryMessage {
    pub message: AgentMessage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_ui_event: Option<ToolUiEvent>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_call_ui_events: Vec<ToolCallUiEvent>,
}

impl HistoryMessage {
    pub fn system(content: impl Into<String>) -> Self {
        let content = content.into();
        Self {
            message: AgentMessage::system(content.clone()),
            tool_ui_event: None,
            tool_call_ui_events: Vec::new(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        let content = content.into();
        Self {
            message: AgentMessage::user(content.clone()),
            tool_ui_event: None,
            tool_call_ui_events: Vec::new(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        let content = content.into();
        Self {
            message: AgentMessage::assistant(content.clone()),
            tool_ui_event: None,
            tool_call_ui_events: Vec::new(),
        }
    }

    pub fn tool(
        tool_call_id: impl Into<String>,
        name: impl Into<String>,
        content: impl Into<String>,
        tool_ui_event: ToolUiEvent,
    ) -> Self {
        let tool_call_id = tool_call_id.into();
        let name = name.into();
        let content = content.into();
        Self {
            message: AgentMessage::tool(tool_call_id, name, content.clone()),
            tool_ui_event: Some(tool_ui_event),
            tool_call_ui_events: Vec::new(),
        }
    }

    pub fn role_name(&self) -> &'static str {
        match &self.message {
            AgentMessage::System { .. } => "system",
            AgentMessage::User { .. } => "user",
            AgentMessage::Assistant { .. } => "assistant",
            AgentMessage::AssistantToolCallProtocol { .. } => "assistant",
            AgentMessage::Tool { .. } => "tool",
        }
    }

    pub fn is_user(&self) -> bool {
        matches!(self.message, AgentMessage::User { .. })
    }

    pub fn is_assistant(&self) -> bool {
        matches!(
            self.message,
            AgentMessage::Assistant { .. } | AgentMessage::AssistantToolCallProtocol { .. }
        )
    }

    pub fn is_system(&self) -> bool {
        matches!(self.message, AgentMessage::System { .. })
    }

    pub fn is_tool(&self) -> bool {
        matches!(self.message, AgentMessage::Tool { .. })
    }

    pub fn text_content(&self) -> Option<&str> {
        match &self.message {
            AgentMessage::System { content } | AgentMessage::Assistant { content } => {
                Some(content.as_str())
            }
            AgentMessage::User { content } => Some(content.as_text()),
            AgentMessage::AssistantToolCallProtocol { content, .. } => content.as_deref(),
            AgentMessage::Tool { content, .. } => Some(content.as_str()),
        }
    }
}

impl PromptRequest {
    fn push_retry_message(&self, message: String) -> Self {
        let mut request = self.clone();
        request.retry_messages.push(HistoryMessage::user(message));
        request
    }

    fn with_schema_retry_note(&self, note: impl Into<String>) -> Self {
        self.push_retry_message(format!(
            "The previous output failed type validation. Fix only the output structure and retry.\n\
Error: {}\n\
Strict requirements:\n\
1. Return exactly one JSON object matching the schema.\n\
2. Do not return markdown, do not use ```json code fences, and do not add explanatory text.\n\
3. Provide every field. If a field is not currently needed, use an empty string, false, 0, or an empty array instead of null, and do not omit it.\n\
4. Enum values must exactly match the schema; do not rewrite their names.\n\
5. If the provider supports tool calls, put this JSON in tool arguments rather than plain text content.",
            note.into()
        ))
    }

    pub(crate) fn with_semantic_retry_note(&self, note: impl Into<String>) -> Self {
        self.push_retry_message(format!(
            "The previous output passed JSON schema validation but failed program semantic validation. Fix the content according to the specific error and retry.\n\
Error: {}\n\
Strict requirements:\n\
1. Keep the result as exactly one JSON object matching the schema.\n\
2. Correct every missing item, duplicate, unknown item, or coverage gap named in the error; do not ignore them.\n\
3. If the error mentions a missing test, group, rule, or field, add it explicitly to the output instead of only implying it elsewhere.\n\
4. Do not delete valid content that already satisfies requirements unless it directly conflicts with the error.\n\
5. Do not return markdown or explanatory text; return only the corrected JSON.",
            note.into()
        ))
    }

    pub fn all_messages(&self) -> Vec<HistoryMessage> {
        let mut messages = self
            .system_messages
            .iter()
            .cloned()
            .map(HistoryMessage::system)
            .collect::<Vec<_>>();
        messages.extend(self.long_term_memory_messages.clone());
        messages.extend(self.history_messages.clone());
        messages.push(HistoryMessage::user(self.current_user_message.clone()));
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
            content: AgentContent::text(content),
        }
    }

    pub fn user_content(content: impl Into<AgentContent>) -> Self {
        Self::User {
            content: content.into(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self::Assistant {
            content: content.into(),
        }
    }

    pub fn assistant_tool_call_protocol_with_reasoning(
        content: Option<String>,
        reasoning_content: Option<String>,
        calls: Vec<AgentToolCall>,
    ) -> Self {
        Self::AssistantToolCallProtocol {
            content,
            reasoning_content,
            calls,
        }
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
    reasoning_content: Option<&str>,
    calls: &[AgentToolCall],
) -> usize {
    content.unwrap_or_default().chars().count()
        + reasoning_content.unwrap_or_default().chars().count()
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
    reasoning_content: Option<&str>,
    calls: &[AgentToolCall],
) -> Vec<String> {
    let mut lines = vec!["role=assistant".to_string()];
    if let Some(content) = content
        && !content.trim().is_empty()
    {
        lines.push("content:".to_string());
        lines.push(content.to_string());
    }
    if reasoning_content.is_some_and(|text| !text.trim().is_empty()) {
        lines.push("reasoning_content=<redacted>".to_string());
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

pub async fn resolve_program_tuning<P: Program>(
    context: &Context,
    program: &P,
) -> PromptTuningConfig<P::Output> {
    if let Some(tuning) = context.compiled_prompts.get_tuning(program) {
        return tuning;
    }

    let tuning = program.default_tuning();
    if let Err(err) = seed_compiled_program_from_tuning_for_model(
        &context.config.main_model_config().model_id,
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

pub async fn execute_program_with_ir_report<P: Program, R: Renderer>(
    llm: &(dyn Llm + Send + Sync),
    context: &Context,
    renderer: &R,
    program: &P,
    ir: super::ir::PromptIR,
    tuning: &PromptTuningConfig<P::Output>,
    trace_origin: TraceOrigin,
) -> Result<ProgramExecutionOutcome<P::Output>> {
    execute_program_with_ir_report_with_retry_hook_and_validator(
        ProgramExecutionRequest {
            llm,
            context,
            renderer,
            program,
            ir,
            tuning,
            trace_origin,
            max_retry_count: DEFAULT_PROGRAM_RETRY_COUNT,
        },
        |_| Ok(()),
        |_| {},
    )
    .await
}

async fn execute_program_with_ir_report_with_retry_hook_and_validator<
    P: Program,
    R: Renderer,
    V,
    F,
>(
    execution: ProgramExecutionRequest<'_, P, R>,
    mut validate_output: V,
    mut on_retry: F,
) -> Result<ProgramExecutionOutcome<P::Output>>
where
    V: FnMut(&P::Output) -> std::result::Result<(), String>,
    F: FnMut(&PromptRequest),
{
    let ProgramExecutionRequest {
        llm,
        context,
        renderer,
        program,
        ir,
        tuning,
        trace_origin,
        max_retry_count,
    } = execution;

    let request = renderer.render(context, program, ir, tuning);
    execute_prompt_request_with_retry_hook_and_validator(
        PromptRequestExecution {
            llm,
            context,
            program_name: program.name(),
            signature: program.signature(),
            request,
            trace_origin,
            max_retry_count,
        },
        &mut validate_output,
        &mut on_retry,
    )
    .await
}

async fn execute_prompt_request_with_retry_hook_and_validator<O, V, F>(
    execution: PromptRequestExecution<'_>,
    validate_output: &mut V,
    on_retry: &mut F,
) -> Result<ProgramExecutionOutcome<O>>
where
    O: DeserializeOwned + Serialize,
    V: FnMut(&O) -> std::result::Result<(), String>,
    F: FnMut(&PromptRequest),
{
    let PromptRequestExecution {
        llm,
        context,
        program_name,
        signature,
        mut request,
        trace_origin,
        max_retry_count,
    } = execution;

    let mut last_error = None;

    for attempt in 0..=max_retry_count {
        let value = match llm.run_json(context, request.clone()).await {
            Ok(value) => value,
            Err(err) => {
                let error_text = err.to_string();
                append_program_trace(ProgramTraceRecord::new(ProgramTraceRecordParts {
                    origin: trace_origin,
                    program_name: program_name.to_string(),
                    attempt: attempt + 1,
                    signature: signature.clone(),
                    request: request.clone(),
                    raw_response: json!({ "provider_error": error_text }),
                    parsed_output: None,
                    deserialization_error: Some(err.to_string()),
                }))
                .await;
                last_error = Some(error_text.clone());
                if attempt < max_retry_count {
                    request = request.with_schema_retry_note(error_text);
                    on_retry(&request);
                }
                continue;
            }
        };
        match serde_json::from_value::<O>(value.clone()) {
            Ok(output) => {
                let parsed_output = serde_json::to_value(&output).ok();
                match validate_output(&output) {
                    Ok(()) => {
                        append_program_trace(ProgramTraceRecord::new(ProgramTraceRecordParts {
                            origin: trace_origin,
                            program_name: program_name.to_string(),
                            attempt: attempt + 1,
                            signature: signature.clone(),
                            request: request.clone(),
                            raw_response: value,
                            parsed_output,
                            deserialization_error: None,
                        }))
                        .await;
                        return Ok(ProgramExecutionOutcome { output });
                    }
                    Err(validation_error) => {
                        append_program_trace(ProgramTraceRecord::new(ProgramTraceRecordParts {
                            origin: trace_origin,
                            program_name: program_name.to_string(),
                            attempt: attempt + 1,
                            signature: signature.clone(),
                            request: request.clone(),
                            raw_response: value,
                            parsed_output,
                            deserialization_error: Some(validation_error.clone()),
                        }))
                        .await;
                        last_error = Some(validation_error.clone());
                        if attempt < max_retry_count {
                            request = request.with_semantic_retry_note(validation_error);
                            on_retry(&request);
                        }
                    }
                }
            }
            Err(err) => {
                last_error = Some(err.to_string());
                append_program_trace(ProgramTraceRecord::new(ProgramTraceRecordParts {
                    origin: trace_origin,
                    program_name: program_name.to_string(),
                    attempt: attempt + 1,
                    signature: signature.clone(),
                    request: request.clone(),
                    raw_response: value,
                    parsed_output: None,
                    deserialization_error: Some(err.to_string()),
                }))
                .await;
                if attempt < max_retry_count {
                    request = request.with_schema_retry_note(err.to_string());
                    on_retry(&request);
                }
            }
        }
    }

    Err(miette!(
        "program {} failed after retries: {}",
        program_name,
        last_error.unwrap_or_else(|| "unknown error".to_string())
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_content_keeps_plain_text_wire_shape() {
        let content = AgentContent::text("hello");

        assert_eq!(serde_json::to_value(&content).unwrap(), json!("hello"));

        let message = AgentMessage::user("hello");
        assert_eq!(
            serde_json::to_value(&message).unwrap(),
            json!({
                "kind": "User",
                "content": "hello"
            })
        );

        let decoded: AgentMessage = serde_json::from_value(json!({
            "kind": "User",
            "content": "hello"
        }))
        .unwrap();
        match decoded {
            AgentMessage::User { content } => {
                assert_eq!(content.as_text(), "hello");
                assert!(content.parts().is_empty());
            }
            _ => panic!("expected user message"),
        }
    }

    #[test]
    fn agent_content_decodes_rich_user_content() {
        let decoded: AgentMessage = serde_json::from_value(json!({
            "kind": "User",
            "content": {
                "text": "look at this",
                "parts": [{
                    "type": "image",
                    "path": "/tmp/demo.png",
                    "media_type": "image/png",
                    "description": "demo image"
                }]
            }
        }))
        .unwrap();

        match decoded {
            AgentMessage::User { content } => {
                assert_eq!(content.as_text(), "look at this");
                assert_eq!(content.parts().len(), 1);
                assert_eq!(
                    content.parts()[0],
                    AgentContentPart::Image {
                        path: "/tmp/demo.png".to_string(),
                        media_type: "image/png".to_string(),
                        description: Some("demo image".to_string()),
                    }
                );
            }
            _ => panic!("expected user message"),
        }
    }
}
