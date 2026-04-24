use super::*;

#[derive(Default, Clone)]
pub(super) struct StreamingToolCallBuilder {
    id: String,
    name: String,
    arguments: String,
}

impl StreamingToolCallBuilder {
    pub(super) fn apply_delta(&mut self, delta: &serde_json::Value) {
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

    pub(super) fn try_build(&self) -> Option<AgentToolCall> {
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

pub(super) fn should_retry_request_without_thinking_budget(body: &str) -> bool {
    let body = body.to_ascii_lowercase();
    body.contains("unknown parameter: 'reasoning'")
        || body.contains("unknown parameter: \"reasoning\"")
        || body.contains("unknown parameter: 'reasoning.effort'")
        || body.contains("unknown parameter: \"reasoning.effort\"")
}

pub(super) fn summarize_agent_turn_request(
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

pub(super) fn summarize_prompt_request(
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
        AgentMessage::System { content }
        | AgentMessage::User { content }
        | AgentMessage::Assistant { content } => content.chars().count(),
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

pub(super) fn format_request_error(
    prefix: &str,
    url: &str,
    request_context: &[String],
    err: &reqwest::Error,
) -> miette::Report {
    let mut lines = vec![format!("{prefix}: {err}"), format!("url={url}")];
    lines.extend(request_context.iter().cloned());
    if err.is_timeout() {
        lines.push("kind=timeout".to_string());
    } else if err.is_connect() {
        lines.push("kind=connect".to_string());
    } else if err.is_request() {
        lines.push("kind=request".to_string());
    } else if err.is_body() {
        lines.push("kind=body".to_string());
    } else if err.is_decode() {
        lines.push("kind=decode".to_string());
    }

    let mut causes = Vec::new();
    let mut current = err.source();
    while let Some(source) = current {
        causes.push(source.to_string());
        current = source.source();
    }
    if !causes.is_empty() {
        lines.push("causes:".to_string());
        lines.extend(causes.into_iter().map(|cause| format!("- {cause}")));
    }

    miette!(lines.join("\n"))
}

pub(super) fn truncate_for_error(text: &str) -> String {
    const MAX_LEN: usize = 600;
    if text.chars().count() <= MAX_LEN {
        return text.to_string();
    }
    let truncated = text.chars().take(MAX_LEN).collect::<String>();
    format!("{truncated}...")
}

pub(super) fn non_empty_string(text: String) -> Option<String> {
    if text.trim().is_empty() {
        None
    } else {
        Some(text)
    }
}

pub(super) fn looks_like_context_window_error(body: &str) -> bool {
    let normalized = body.to_ascii_lowercase();
    normalized.contains("context length")
        || normalized.contains("context window")
        || normalized.contains("maximum context length")
        || normalized.contains("too many tokens")
        || normalized.contains("max context")
}

pub(super) fn truncate_for_json_error(value: &serde_json::Value) -> String {
    truncate_for_error(&value.to_string())
}

pub(super) fn parse_retry_after_seconds(value: &str) -> Option<u64> {
    value.trim().parse::<u64>().ok()
}

pub(super) fn default_rate_limit_backoff(attempt: usize) -> Duration {
    let seconds = match attempt {
        0 => 2,
        1 => 4,
        2 => 8,
        _ => 12,
    };
    Duration::from_secs(seconds)
}
