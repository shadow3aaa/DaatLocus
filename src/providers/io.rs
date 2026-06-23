use super::*;

#[derive(Default, Clone)]
pub(crate) struct StreamingToolCallBuilder {
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

pub(super) fn should_retry_prompt_request_with_string_tool_choice(body: &str) -> bool {
    let body = body.to_ascii_lowercase();
    body.contains("unknown parameter: 'tool_choice.function'")
        || body.contains("unknown parameter: \"tool_choice.function\"")
}

pub(super) fn should_retry_prompt_request_without_tool_choice(body: &str) -> bool {
    let body = body.to_ascii_lowercase();
    body.contains("does not support this tool_choice")
        || body.contains("does not support tool_choice")
        || body.contains("unknown parameter: 'tool_choice'")
        || body.contains("unknown parameter: \"tool_choice\"")
}

pub(super) fn should_retry_prompt_request_with_nested_thinking_budget(body: &str) -> bool {
    let body = body.to_ascii_lowercase();
    body.contains("unknown parameter: 'reasoning_effort'")
        || body.contains("unknown parameter: \"reasoning_effort\"")
}

pub(crate) fn should_retry_request_without_thinking_budget(body: &str) -> bool {
    let body = body.to_ascii_lowercase();
    body.contains("unknown parameter: 'reasoning'")
        || body.contains("unknown parameter: \"reasoning\"")
        || body.contains("unknown parameter: 'reasoning.effort'")
        || body.contains("unknown parameter: \"reasoning.effort\"")
}

pub(crate) fn should_retry_request_without_reasoning_summary(body: &str) -> bool {
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

pub(crate) fn should_retry_request_without_reasoning_content(body: &str) -> bool {
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
pub(crate) fn looks_like_vision_unsupported_error(body: &str) -> bool {
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

pub(crate) fn summarize_agent_turn_request(
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

pub(crate) fn summarize_prompt_request(
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
                                    .map(|text| text.chars().count())
                                    .unwrap_or(0)
                        }
                    })
                    .sum::<usize>()
        }
        AgentMessage::AssistantToolCallProtocol {
            content,
            reasoning_content,
            calls,
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
        .map(|text| text.to_string())
        .unwrap_or_default();
    let reasoning_content = message["reasoning_content"]
        .as_str()
        .map(|text| text.to_string())
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
    })
}

pub(super) fn parse_usage_from_response_json(
    response_json: &serde_json::Value,
) -> Option<TokenUsage> {
    let usage = response_json.get("usage")?;
    let input_tokens = usage
        .get("prompt_tokens")
        .and_then(|value| value.as_i64())
        .unwrap_or_default();
    let output_tokens = usage
        .get("completion_tokens")
        .and_then(|value| value.as_i64())
        .unwrap_or_default();
    let total_tokens = usage
        .get("total_tokens")
        .and_then(|value| value.as_i64())
        .unwrap_or_else(|| input_tokens + output_tokens);
    let cached_input_tokens = usage
        .get("prompt_tokens_details")
        .and_then(|value| value.get("cached_tokens"))
        .and_then(|value| value.as_i64())
        .unwrap_or_default();
    let reasoning_output_tokens = usage
        .get("completion_tokens_details")
        .and_then(|value| value.get("reasoning_tokens"))
        .and_then(|value| value.as_i64())
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

pub(super) fn normalize_sse_buffer(buffer: &mut String) {
    if buffer.contains('\r') {
        *buffer = buffer.replace("\r\n", "\n").replace('\r', "\n");
    }
}

pub(super) fn take_next_sse_event(buffer: &mut String) -> Option<String> {
    let delimiter_index = buffer.find("\n\n")?;
    let event = buffer[..delimiter_index].to_string();
    buffer.drain(..delimiter_index + 2);
    Some(event)
}

pub(crate) fn format_request_error(
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

pub(crate) async fn send_request_for_streaming_response(
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

pub(crate) async fn read_response_text_with_timeout(
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

pub(crate) fn truncate_for_error(text: &str) -> String {
    const MAX_LEN: usize = 600;
    if text.chars().count() <= MAX_LEN {
        return text.to_string();
    }
    let truncated = text.chars().take(MAX_LEN).collect::<String>();
    format!("{truncated}...")
}

pub(crate) fn non_empty_string(text: String) -> Option<String> {
    if text.trim().is_empty() {
        None
    } else {
        Some(text)
    }
}

pub(crate) fn looks_like_context_window_error(body: &str) -> bool {
    let normalized = body.to_ascii_lowercase();
    normalized.contains("context length")
        || normalized.contains("context window")
        || normalized.contains("maximum context length")
        || normalized.contains("too many tokens")
        || normalized.contains("max context")
}

pub(crate) fn truncate_for_json_error(value: &serde_json::Value) -> String {
    truncate_for_error(&value.to_string())
}

pub(crate) fn parse_retry_after_seconds(value: &str) -> Option<u64> {
    value.trim().parse::<u64>().ok()
}

pub(crate) fn default_rate_limit_backoff(attempt: usize) -> Duration {
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
}
