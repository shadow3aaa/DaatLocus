use sha2::{Digest, Sha256};

use crate::context_budget::RequestBudgetBreakdown;

use super::*;

#[derive(Default, Clone)]
pub struct StreamingToolCallBuilder {
    id: String,
    name: String,
    arguments: String,
}

impl StreamingToolCallBuilder {
    pub(crate) fn apply_delta(&mut self, delta: &serde_json::Value) {
        if let Some(id) = delta["id"].as_str() {
            self.id.push_str(id);
        }
        if let Some(name) = delta["function"]["name"].as_str() {
            self.name.push_str(name);
        }
        if let Some(arguments) = delta["function"]["arguments"].as_str() {
            self.arguments.push_str(arguments);
        }
    }

    /// A builder that never received any `tool_call` delta content. Some
    /// OpenAI-compatible proxies (e.g. Anthropic models fronted by a gateway)
    /// emit `tool_call` deltas with a non-zero `index`, leaving lower indices as
    /// empty placeholders that must be skipped rather than treated as
    /// incomplete calls.
    pub(crate) const fn is_empty(&self) -> bool {
        self.id.is_empty() && self.name.is_empty() && self.arguments.is_empty()
    }

    pub(crate) fn try_build(&self) -> Option<AgentToolCall> {
        if self.id.is_empty() || self.name.is_empty() {
            return None;
        }
        let arguments = serde_json::from_str(&self.arguments).ok()?;
        Some(AgentToolCall {
            id: self.id.clone(),
            name: self.name.clone(),
            arguments,
        })
    }
}
pub(super) fn should_retry_prompt_request_with_nested_thinking_budget(body: &str) -> bool {
    let body = body.to_ascii_lowercase();
    body.contains("unknown parameter: 'reasoning_effort'")
        || body.contains("unknown parameter: \"reasoning_effort\"")
}

pub fn should_retry_request_without_thinking_budget(body: &str) -> bool {
    let body = body.to_ascii_lowercase();
    body.contains("unknown parameter: 'reasoning'")
        || body.contains("unknown parameter: \"reasoning\"")
        || body.contains("unknown parameter: 'reasoning.effort'")
        || body.contains("unknown parameter: \"reasoning.effort\"")
}

pub fn should_retry_request_without_reasoning_summary(body: &str) -> bool {
    let body = body.to_ascii_lowercase();
    body.contains("reasoning.summary")
        || (body.contains("reasoning")
            && body.contains("summary")
            && (body.contains("unsupported")
                || body.contains("not supported")
                || body.contains("unknown parameter")
                || body.contains("unknown field")
                || body.contains("unrecognized parameter")
                || body.contains("unrecognized field")
                || body.contains("invalid")
                || body.contains("not permitted")
                || body.contains("verified")))
}

pub fn should_retry_request_without_reasoning_content(body: &str) -> bool {
    let body = body.to_ascii_lowercase();
    body.contains("reasoning_content")
        && (body.contains("unknown parameter")
            || body.contains("unknown field")
            || body.contains("extra_forbidden")
            || body.contains("extra inputs are not permitted")
            || body.contains("unrecognized parameter")
            || body.contains("unrecognized field")
            || body.contains("invalid message field"))
}

/// Returns `true` when the provider error indicates the model does not accept
/// `image_url` (or `input_image`) content blocks.
pub fn looks_like_vision_unsupported_error(body: &str) -> bool {
    let body = body.to_ascii_lowercase();
    // Providers that use OpenAI-compatible deserialization emit this when an
    // enum variant (image_url / input_image) is unknown.
    (body.contains("image_url") || body.contains("input_image"))
        && body.contains("unknown variant")
        // Generic "does not support image/vision" messages from various providers
        || body.contains("does not support image")
        || body.contains("does not support vision")
        || (body.contains("vision") && body.contains("not supported"))
}

pub fn summarize_agent_turn_request(
    request: &AgentTurnRequest,
    budget: Option<&RequestBudgetBreakdown>,
) -> Vec<String> {
    let message_count = request.messages.len();
    let tool_count = request.tools.len();
    let message_chars = request
        .messages
        .iter()
        .map(agent_message_char_count)
        .sum::<usize>();
    let tool_names = request
        .tools
        .iter()
        .take(8)
        .map(|tool| tool.name.clone())
        .collect::<Vec<_>>();
    let mut lines = vec![
        format!("message_count={message_count}"),
        format!("tool_count={tool_count}"),
        format!("message_chars={message_chars}"),
        format!(
            "tools={}",
            if tool_names.is_empty() {
                "<none>".to_string()
            } else {
                tool_names.join(", ")
            }
        ),
    ];
    if let Some(budget) = budget {
        lines.extend(budget.summary_lines());
    }
    lines
}

pub fn summarize_prompt_request(
    request: &PromptRequest,
    budget: Option<&RequestBudgetBreakdown>,
) -> Vec<String> {
    let mut lines = vec![
        format!("message_count={}", request.all_messages().len()),
        format!("tool_name={}", request.tool_name),
    ];
    if let Some(budget) = budget {
        lines.extend(budget.summary_lines());
    }
    lines
}

fn agent_message_char_count(message: &AgentMessage) -> usize {
    match message {
        AgentMessage::System { content } | AgentMessage::Assistant { content } => {
            content.chars().count()
        }
        AgentMessage::User { content } => {
            content.as_text().chars().count()
                + content
                    .parts()
                    .iter()
                    .map(|part| match part {
                        AgentContentPart::Text { text } => text.chars().count(),
                        AgentContentPart::Image {
                            path,
                            media_type,
                            description,
                        } => {
                            path.chars().count()
                                + media_type.chars().count()
                                + description
                                    .as_deref()
                                    .map_or(0, |text| text.chars().count())
                        }
                    })
                    .sum::<usize>()
        }
        AgentMessage::AssistantToolCallProtocol {
            content,
            reasoning_content,
            calls,
            ..
        } => assistant_tool_call_protocol_char_count(
            content.as_deref(),
            reasoning_content.as_deref(),
            calls,
        ),
        AgentMessage::Tool {
            tool_call_id,
            name,
            content,
        } => tool_call_id.chars().count() + name.chars().count() + content.chars().count(),
    }
}

pub(super) fn parse_agent_turn_stream_result_from_json(
    response_json: &serde_json::Value,
) -> Result<AgentTurnStreamResult> {
    let message = &response_json["choices"][0]["message"];
    let content = message["content"]
        .as_str()
        .map(std::string::ToString::to_string)
        .unwrap_or_default();
    let reasoning_content = message["reasoning_content"]
        .as_str()
        .map(std::string::ToString::to_string)
        .filter(|text| !text.trim().is_empty());

    if let Some(tool_calls) = message["tool_calls"].as_array()
        && !tool_calls.is_empty()
    {
        let mut calls = Vec::new();
        for tool_call in tool_calls {
            let id = tool_call["id"].as_str().ok_or_else(|| {
                miette!(
                    "llm response missing tool_call.id; response={}",
                    truncate_for_json_error(response_json)
                )
            })?;
            let name = tool_call["function"]["name"].as_str().ok_or_else(|| {
                miette!(
                    "llm response missing tool function name; response={}",
                    truncate_for_json_error(response_json)
                )
            })?;
            let arguments_str = tool_call["function"]["arguments"].as_str().ok_or_else(|| {
                miette!(
                    "llm response missing tool function arguments; response={}",
                    truncate_for_json_error(response_json)
                )
            })?;
            let arguments = serde_json::from_str(arguments_str).map_err(|err| {
                miette!(
                    "failed to decode tool arguments as JSON: {err}; arguments={}",
                    truncate_for_error(arguments_str)
                )
            })?;
            calls.push(AgentToolCall {
                id: id.to_string(),
                name: name.to_string(),
                arguments,
            });
        }
        let assistant_message = if content.trim().is_empty() {
            None
        } else {
            Some(content)
        };
        let mut items = Vec::with_capacity(calls.len() + usize::from(assistant_message.is_some()));
        if let Some(content) = assistant_message.clone() {
            items.push(AgentTurnItem::AssistantMessage { content });
        }
        items.extend(
            calls
                .into_iter()
                .map(|call| AgentTurnItem::ToolCall { call }),
        );
        return Ok(AgentTurnStreamResult {
            items,
            raw_stream_follow_up: true,
            last_assistant_message: assistant_message,
            last_reasoning_content: reasoning_content,
            last_reasoning_signature: None,
        });
    }

    let last_assistant_message = if content.trim().is_empty() {
        None
    } else {
        Some(content)
    };
    Ok(AgentTurnStreamResult {
        items: last_assistant_message
            .clone()
            .into_iter()
            .map(|content| AgentTurnItem::AssistantMessage { content })
            .collect(),
        raw_stream_follow_up: false,
        last_assistant_message,
        last_reasoning_content: reasoning_content,
        last_reasoning_signature: None,
    })
}

pub(super) fn parse_usage_from_response_json(
    response_json: &serde_json::Value,
) -> Option<TokenUsage> {
    let usage = response_json.get("usage")?;
    let input_tokens = usage
        .get("prompt_tokens")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or_default();
    let output_tokens = usage
        .get("completion_tokens")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or_default();
    let total_tokens = usage
        .get("total_tokens")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or_else(|| input_tokens + output_tokens);
    let cached_input_tokens = usage
        .get("prompt_tokens_details")
        .and_then(|value| value.get("cached_tokens"))
        .and_then(serde_json::Value::as_i64)
        .unwrap_or_default();
    let reasoning_output_tokens = usage
        .get("completion_tokens_details")
        .and_then(|value| value.get("reasoning_tokens"))
        .and_then(serde_json::Value::as_i64)
        .unwrap_or_default();
    let usage = TokenUsage {
        input_tokens,
        cached_input_tokens,
        output_tokens,
        reasoning_output_tokens,
        total_tokens,
    };
    if usage.is_zero() { None } else { Some(usage) }
}

const SSE_CURSOR_HEADER: usize = 8;
const SSE_CURSOR_MAGIC: u32 = 0x444c_5343;

fn framed(buffer: &[u8]) -> bool {
    buffer.len() >= SSE_CURSOR_HEADER
        && buffer[..4] == SSE_CURSOR_MAGIC.to_le_bytes()
}

fn sse_cursor(buffer: &[u8]) -> usize {
    if !framed(buffer) {
        return 0;
    }
    let cursor = u32::from_le_bytes(buffer[4..8].try_into().unwrap_or([0; 4])) as usize;
    if cursor <= buffer.len() - SSE_CURSOR_HEADER {
        cursor
    } else {
        0
    }
}

fn write_sse_cursor(buffer: &mut Vec<u8>, cursor: usize) {
    let cursor = u32::try_from(cursor).unwrap_or(u32::MAX);
    if framed(buffer) {
        buffer[4..8].copy_from_slice(&cursor.to_le_bytes());
        return;
    }
    let mut framed_buffer = Vec::with_capacity(SSE_CURSOR_HEADER + buffer.len());
    framed_buffer.extend_from_slice(&SSE_CURSOR_MAGIC.to_le_bytes());
    framed_buffer.extend_from_slice(&cursor.to_le_bytes());
    framed_buffer.extend_from_slice(buffer);
    *buffer = framed_buffer;
}

fn sse_bytes(buffer: &mut Vec<u8>) -> &mut [u8] {
    if framed(buffer) {
        &mut buffer[SSE_CURSOR_HEADER..]
    } else {
        buffer
    }
}

pub(super) fn normalize_sse_buffer(buffer: &mut Vec<u8>) {
    let cursor = sse_cursor(buffer);
    let needs_rewrite = sse_bytes(buffer).contains(&b'\r');
    if !needs_rewrite {
        return;
    }
    let payload = sse_bytes(buffer).to_vec();
    // Replace \r\n with \n and bare \r with \n, operating on raw bytes.
    let mut out = Vec::with_capacity(payload.len());
    let mut i = 0;
    while i < payload.len() {
        if payload[i] == b'\r' {
            out.push(b'\n');
            i += if i + 1 < payload.len() && payload[i + 1] == b'\n' {
                2
            } else {
                1
            };
        } else {
            out.push(payload[i]);
            i += 1;
        }
    }
    *buffer = out;
    write_sse_cursor(buffer, cursor.min(buffer.len()));
}

/// Byte cursor over an SSE buffer so consuming an event does not shift the tail.
#[derive(Default)]
pub(super) struct SseBuffer {
    bytes: Vec<u8>,
    cursor: usize,
}

impl SseBuffer {
    pub(super) fn extend_from_slice(&mut self, chunk: &[u8]) {
        self.compact_if_needed();
        self.bytes.extend_from_slice(chunk);
    }

    pub(super) fn normalize(&mut self) {
        if self.cursor == 0 {
            normalize_sse_buffer(&mut self.bytes);
            return;
        }
        if !self.bytes[self.cursor..].contains(&b'\r') {
            return;
        }
        let mut pending = self.bytes.split_off(self.cursor);
        self.cursor = 0;
        self.bytes.clear();
        normalize_sse_buffer(&mut pending);
        self.bytes = pending;
    }

    pub(super) fn next_event(&mut self) -> Option<String> {
        let pending = self.bytes.get(self.cursor..)?;
        let delimiter_index = pending.windows(2).position(|window| window == b"\n\n")?;
        let event = String::from_utf8_lossy(&pending[..delimiter_index]).into_owned();
        self.cursor += delimiter_index + 2;
        Some(event)
    }

    fn compact_if_needed(&mut self) {
        if self.cursor == 0 {
            return;
        }
        if self.cursor == self.bytes.len() || self.cursor >= 4096 {
            self.bytes.drain(..self.cursor);
            self.cursor = 0;
        }
    }
}

pub(super) fn take_next_sse_event(buffer: &mut Vec<u8>) -> Option<String> {
    let mut cursor = sse_cursor(buffer);
    let base = usize::from(framed(buffer)) * SSE_CURSOR_HEADER;
    let payload = &buffer[base..];
    let delimiter_index = payload
        .get(cursor..)?
        .windows(2)
        .position(|window| window == b"\n\n")?;
    let event = String::from_utf8_lossy(&payload[cursor..cursor + delimiter_index]).into_owned();
    cursor += delimiter_index + 2;
    if cursor >= 4096 {
        let rest = buffer[base + cursor..].to_vec();
        *buffer = rest;
    } else {
        write_sse_cursor(buffer, cursor);
    }
    Some(event)
}

pub fn format_request_error(
    prefix: &str,
    url: &str,
    request_context: &[String],
    err: &reqwest::Error,
) -> miette::Report {
    let causes = request_error_causes(err);
    let kind = classify_request_error(
        RequestErrorFlags::from_reqwest(err),
        &causes,
        request_context,
    );
    let mut lines = vec![
        request_error_headline(prefix, kind, &err.to_string()),
        format!("url={url}"),
    ];
    lines.extend(request_context.iter().cloned());
    lines.push(format!("kind={kind}"));
    if !causes.is_empty() {
        lines.push("causes:".to_string());
        lines.extend(causes.into_iter().map(|cause| format!("- {cause}")));
    }

    miette!(lines.join("\n"))
}

#[derive(Clone, Copy, Debug, Default)]
struct RequestErrorFlags {
    timeout: bool,
    connect: bool,
    request: bool,
    body: bool,
    decode: bool,
}

impl RequestErrorFlags {
    fn from_reqwest(err: &reqwest::Error) -> Self {
        Self {
            timeout: err.is_timeout(),
            connect: err.is_connect(),
            request: err.is_request(),
            body: err.is_body(),
            decode: err.is_decode(),
        }
    }
}

fn request_error_causes(err: &reqwest::Error) -> Vec<String> {
    let mut causes = Vec::new();
    let mut current = err.source();
    while let Some(source) = current {
        causes.push(source.to_string());
        current = source.source();
    }
    causes
}

fn classify_request_error(
    flags: RequestErrorFlags,
    causes: &[String],
    request_context: &[String],
) -> &'static str {
    if flags.timeout {
        return "timeout";
    }
    if flags.connect {
        return "connect";
    }
    if flags.request {
        return "request";
    }
    if flags.body {
        return stream_or_body_error_kind(request_context);
    }
    if flags.decode && causes_indicate_connection_body_read(causes) {
        return stream_or_body_error_kind(request_context);
    }
    if flags.decode {
        return "decode";
    }
    "unknown"
}

fn stream_or_body_error_kind(request_context: &[String]) -> &'static str {
    if request_context
        .iter()
        .any(|line| line.contains("phase=") && line.contains("stream"))
    {
        "stream_body_read"
    } else {
        "body_read"
    }
}

fn causes_indicate_connection_body_read(causes: &[String]) -> bool {
    causes.iter().any(|cause| {
        let cause = cause.to_ascii_lowercase();
        cause.contains("error reading a body from connection")
            || cause.contains("without sending tls close_notify")
            || cause.contains("unexpected eof")
    })
}

fn request_error_headline(prefix: &str, kind: &str, source: &str) -> String {
    match kind {
        "stream_body_read" => format!("{prefix}: streaming response body read failed"),
        "body_read" => format!("{prefix}: response body read failed"),
        _ => format!("{prefix}: {source}"),
    }
}

pub async fn send_request_for_streaming_response(
    request: reqwest::RequestBuilder,
    timeout: Duration,
    prefix: &str,
    url: &str,
    request_context: &[String],
) -> miette::Result<reqwest::Response> {
    match tokio::time::timeout(timeout, request.send()).await {
        Ok(Ok(response)) => Ok(response),
        Ok(Err(err)) => Err(format_request_error(prefix, url, request_context, &err)),
        Err(_) => {
            let mut lines = vec![
                format!(
                    "{prefix}: response headers timed out after {}s",
                    timeout.as_secs()
                ),
                format!("url={url}"),
            ];
            lines.extend(request_context.iter().cloned());
            lines.push("kind=response_header_timeout".to_string());
            Err(miette!(lines.join("\n")))
        }
    }
}

pub async fn read_response_text_with_timeout(
    response: reqwest::Response,
    timeout: Duration,
    prefix: &str,
    url: &str,
    request_context: &[String],
) -> miette::Result<String> {
    match tokio::time::timeout(timeout, response.text()).await {
        Ok(Ok(body)) => Ok(body),
        Ok(Err(err)) => Err(format_request_error(prefix, url, request_context, &err)),
        Err(_) => {
            let mut lines = vec![
                format!(
                    "{prefix}: response body timed out after {}s",
                    timeout.as_secs()
                ),
                format!("url={url}"),
            ];
            lines.extend(request_context.iter().cloned());
            lines.push("kind=response_body_timeout".to_string());
            Err(miette!(lines.join("\n")))
        }
    }
}

pub fn truncate_for_error(text: &str) -> String {
    const MAX_LEN: usize = 600;
    if text.chars().count() <= MAX_LEN {
        return text.to_string();
    }
    let truncated = text.chars().take(MAX_LEN).collect::<String>();
    format!("{truncated}...")
}

pub fn non_empty_string(text: String) -> Option<String> {
    if text.trim().is_empty() {
        None
    } else {
        Some(text)
    }
}

pub fn looks_like_context_window_error(body: &str) -> bool {
    let normalized = body.to_ascii_lowercase();
    normalized.contains("context length")
        || normalized.contains("context window")
        || normalized.contains("maximum context length")
        || normalized.contains("too many tokens")
        || normalized.contains("max context")
}

pub fn truncate_for_json_error(value: &serde_json::Value) -> String {
    truncate_for_error(&value.to_string())
}

/// OpenAI's Responses API rejects `call_id` values longer than 64 characters.
/// Tool-call ids inherited from other providers can exceed that, so fold long
/// ids into a deterministic, collision-resistant form that stays within the
/// limit; the same id is then used for both `function_call` and
/// `function_call_output` items.
pub fn responses_safe_call_id(call_id: &str) -> String {
    const MAX_CALL_ID_LEN: usize = 64;
    const DIGEST_LEN: usize = 32;
    if call_id.len() <= MAX_CALL_ID_LEN {
        return call_id.to_string();
    }
    let digest = hex::encode(Sha256::digest(call_id.as_bytes()));
    let prefix_budget = MAX_CALL_ID_LEN - DIGEST_LEN - 1;
    let mut prefix = String::new();
    for ch in call_id.chars() {
        if prefix.len() + ch.len_utf8() > prefix_budget {
            break;
        }
        prefix.push(ch);
    }
    format!("{prefix}#{}", &digest[..DIGEST_LEN])
}

pub fn parse_retry_after_seconds(value: &str) -> Option<u64> {
    value.trim().parse::<u64>().ok()
}

pub const fn default_rate_limit_backoff(attempt: usize) -> Duration {
    let seconds = match attempt {
        0 => 2,
        1 => 4,
        2 => 8,
        _ => 12,
    };
    Duration::from_secs(seconds)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_reasoning_summary_rejection_errors() {
        assert!(should_retry_request_without_reasoning_summary(
            r#"{"error":{"message":"Your organization must be verified to generate reasoning summaries.","param":"reasoning.summary","code":"unsupported_value"}}"#
        ));
        assert!(should_retry_request_without_reasoning_summary(
            "Unknown parameter: 'reasoning.summary'."
        ));
        assert!(!should_retry_request_without_reasoning_summary(
            "The assistant summary mentioned reasoning, but the request succeeded."
        ));
    }

    #[test]
    fn classifies_decode_wrapped_stream_body_read_errors() {
        let kind = classify_request_error(
            RequestErrorFlags {
                decode: true,
                ..RequestErrorFlags::default()
            },
            &[
                "error reading a body from connection".to_string(),
                "peer closed connection without sending TLS close_notify".to_string(),
            ],
            &["phase=response_stream".to_string()],
        );

        assert_eq!(kind, "stream_body_read");
    }

    #[test]
    fn classifies_regular_decode_errors_as_decode() {
        let kind = classify_request_error(
            RequestErrorFlags {
                decode: true,
                ..RequestErrorFlags::default()
            },
            &["expected value at line 1 column 1".to_string()],
            &["phase=response_stream".to_string()],
        );

        assert_eq!(kind, "decode");
    }

    #[test]
    fn stream_body_read_headline_hides_decode_wrapper() {
        let headline = request_error_headline(
            "Codex Responses stream read failed",
            "stream_body_read",
            "error decoding response body",
        );

        assert_eq!(
            headline,
            "Codex Responses stream read failed: streaming response body read failed"
        );
        assert!(!headline.contains("decoding"));
    }

    #[test]
    fn default_tool_call_builder_is_empty_and_populated_one_is_not() {
        let empty = StreamingToolCallBuilder::default();
        assert!(empty.is_empty());
        assert!(empty.try_build().is_none());

        let mut builder = StreamingToolCallBuilder::default();
        builder.apply_delta(&serde_json::json!({
            "id": "toolu_1",
            "function": { "name": "read_file", "arguments": "{\"path\": \"/tmp/x\"}" }
        }));
        assert!(!builder.is_empty());
        let call = builder.try_build().expect("populated builder should build");
        assert_eq!(call.name, "read_file");
        assert_eq!(call.arguments["path"], "/tmp/x");
    }

    #[test]
    fn sse_cursor_survives_the_next_appended_chunk() {
        let mut buffer = Vec::new();
        buffer.extend_from_slice(b"data: one\n\ndata: tw");
        normalize_sse_buffer(&mut buffer);
        assert_eq!(take_next_sse_event(&mut buffer).as_deref(), Some("data: one"));
        assert!(take_next_sse_event(&mut buffer).is_none());

        buffer.extend_from_slice(b"o\n\n");
        normalize_sse_buffer(&mut buffer);
        assert_eq!(take_next_sse_event(&mut buffer).as_deref(), Some("data: two"));
        assert!(take_next_sse_event(&mut buffer).is_none());

        let mut tight = Vec::new();
        tight.extend_from_slice(b"data: one\n\ndata: tw");
        normalize_sse_buffer(&mut tight);
        assert_eq!(take_next_sse_event(&mut tight).as_deref(), Some("data: one"));
        let fill = tight.capacity().saturating_sub(tight.len()).saturating_sub(1);
        tight.extend(std::iter::repeat_n(b'x', fill));
        assert!(tight.capacity() - tight.len() < 4);
        tight.extend_from_slice(b"\n\n");
        normalize_sse_buffer(&mut tight);
        let mut events = Vec::new();
        while let Some(event) = take_next_sse_event(&mut tight) {
            events.push(event);
        }
        assert_eq!(events.len(), 1);
        assert!(events[0].starts_with("data: tw"));
        assert!(events[0].ends_with("x".repeat(fill).as_str()));
        assert!(!events[0].contains("data: one"));

        let mut growing = Vec::with_capacity(b"data: one\n\ndata: tw".len());
        growing.extend_from_slice(b"data: one\n\ndata: tw");
        normalize_sse_buffer(&mut growing);
        assert_eq!(take_next_sse_event(&mut growing).as_deref(), Some("data: one"));
        growing.extend(std::iter::repeat_n(b'y', growing.capacity()));
        growing.extend_from_slice(b"\n\n");
        normalize_sse_buffer(&mut growing);
        events.clear();
        while let Some(event) = take_next_sse_event(&mut growing) {
            events.push(event);
        }
        assert_eq!(events.len(), 1);
        assert!(events[0].starts_with("data: tw"));
        assert!(!events.iter().any(|event| event.contains("data: one")));
    }
}
