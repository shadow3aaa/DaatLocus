//! Responses wire protocol shared by API-key and ChatGPT-authenticated clients.

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use futures_util::StreamExt;
use miette::{Result, miette};

use serde_json::{Value, json};

use crate::{
    core::{ModelProgressSink, TokenUsage},
    model_protocol::{
        AgentContent, AgentContentPart, AgentMessage, AgentToolCall, AgentToolInputSpec,
        AgentToolSpec, AgentTurnItem, AgentTurnStreamResult, HistoryMessage,
    },
};

use super::{
    flatten_tool_result_as_assistant_text,
    io::{
        format_request_error, non_empty_string, responses_safe_call_id, truncate_for_error,
        truncate_for_json_error,
    },
    payload::image_part_data_url,
};

pub(super) fn reasoning_payload(effort: Option<&str>, request_summary: bool) -> Option<Value> {
    let summary = request_summary && !effort.is_some_and(super::thinking::thinking_budget_is_none);
    if effort.is_none() && !summary {
        return None;
    }
    let mut reasoning = serde_json::Map::new();
    if let Some(effort) = effort {
        reasoning.insert("effort".into(), json!(effort));
    }
    if summary {
        reasoning.insert("summary".into(), json!("auto"));
    }
    Some(Value::Object(reasoning))
}

pub(super) fn messages_to_responses_parts(
    messages: &[AgentMessage],
    strip_images: bool,
) -> (String, Vec<Value>) {
    let mut instructions = Vec::new();
    let mut input = Vec::new();
    let mut call_types = HashMap::new();
    for message in messages {
        // Replay the wire items verbatim: rebuilding them from display text loses
        // message phases, encrypted reasoning, and custom tool input formats.
        if let Some(output) = message.responses_output() {
            for item in output {
                if let Some(call_id) = item["call_id"].as_str() {
                    call_types.insert(call_id.to_string(), item["type"] == "custom_tool_call");
                }
                let mut item = item.clone();
                if let Some(call_id) = item["call_id"].as_str() {
                    item["call_id"] = json!(responses_safe_call_id(call_id));
                }
                input.push(item);
            }
            continue;
        }
        match message {
            AgentMessage::System { content } => instructions.push(content.clone()),
            AgentMessage::User { content } => {
                input.push(responses_user_message(content, strip_images))
            }
            AgentMessage::Assistant { content, .. } => input.push(assistant_message(content)),
            AgentMessage::AssistantToolCallProtocol { content, calls, .. } => {
                if let Some(content) = content.as_deref().filter(|text| !text.trim().is_empty()) {
                    input.push(assistant_message(content));
                }
                for call in calls {
                    let custom = call.arguments.is_string();
                    call_types.insert(call.id.clone(), custom);
                    input.push(if custom {
                        json!({
                            "type": "custom_tool_call",
                            "call_id": responses_safe_call_id(&call.id),
                            "name": call.name,
                            "input": call.arguments.as_str().unwrap_or_default(),
                        })
                    } else {
                        json!({
                            "type": "function_call",
                            "call_id": responses_safe_call_id(&call.id),
                            "name": call.name,
                            "arguments": call.arguments.to_string(),
                        })
                    });
                }
            }
            AgentMessage::Tool {
                tool_call_id,
                name,
                content,
            } => {
                if let Some(custom) = call_types.get(tool_call_id) {
                    input.push(json!({
                        "type": if *custom { "custom_tool_call_output" } else { "function_call_output" },
                        "call_id": responses_safe_call_id(tool_call_id),
                        "output": content,
                    }));
                } else {
                    input.push(assistant_message(&flatten_tool_result_as_assistant_text(
                        name, content,
                    )));
                }
            }
        }
    }
    (instructions.join("\n\n"), input)
}

fn assistant_message(content: &str) -> Value {
    json!({ "type": "message", "role": "assistant", "content": [{ "type": "output_text", "text": content }] })
}

/// Only output domains supported by the runtime are persisted for replay.
pub(super) fn replayable_output_item(item: &Value) -> bool {
    matches!(
        item["type"].as_str(),
        Some("message" | "reasoning" | "function_call" | "custom_tool_call")
    )
}

pub(super) fn output_needs_follow_up(output: &[Value], end_turn: Option<bool>) -> bool {
    end_turn == Some(false)
        || matches!(
            output
                .iter()
                .rev()
                .find(|item| item["type"] == "message")
                .and_then(|item| item["phase"].as_str()),
            Some("commentary" | "partial_answer")
        )
}

pub(super) fn tool_to_responses_tool(tool: AgentToolSpec) -> Value {
    match tool.input_spec {
        AgentToolInputSpec::JsonSchema { schema } => json!({
            "type": "function", "name": tool.name, "description": tool.description,
            "strict": crate::schema_utils::supports_strict_schema(&schema), "parameters": schema,
        }),
        AgentToolInputSpec::FreeformGrammar {
            syntax,
            definition,
            fallback_schema,
        } => {
            if matches!(syntax.as_str(), "lark" | "regex") {
                json!({
                    "type": "custom", "name": tool.name, "description": tool.description,
                    "format": { "type": "grammar", "syntax": syntax, "definition": definition },
                })
            } else {
                json!({
                    "type": "function", "name": tool.name,
                    "description": format!("{}\n\nPut the complete freeform tool input in the `input` field.\nsyntax={syntax}\ndefinition=\n{definition}", tool.description),
                    "strict": crate::schema_utils::supports_strict_schema(&fallback_schema), "parameters": fallback_schema,
                })
            }
        }
    }
}

pub(super) async fn parse_stream(
    response: reqwest::Response,
    progress: Option<&ModelProgressSink>,
    stream_idle_timeout: Option<Duration>,
    model: &str,
    url: &str,
    diagnostics: &mut super::stream_diagnostics::StreamDiagnostics,
    mut record_usage: impl FnMut(TokenUsage),
) -> Result<AgentTurnStreamResult> {
    let mut buffer = super::io::SseBuffer::default();
    let mut delta_content = String::new();
    let mut output_messages = Vec::new();
    let mut reasoning_content = String::new();
    let mut reasoning_item_content = String::new();
    let mut tool_calls = Vec::new();
    let mut responses_output = Vec::new();
    let mut completed = false;
    let mut response_end_turn = None;
    let mut last_assistant_progress_emit_at = Instant::now();
    let mut assistant_char_len = 0usize;
    let mut last_assistant_progress_char_len = 0usize;
    let mut last_assistant_progress_byte_len = 0usize;
    let mut last_reasoning_progress_emit_at = Instant::now();
    let mut reasoning_char_len = 0usize;
    let mut last_reasoning_progress_char_len = 0usize;
    let mut last_reasoning_progress_byte_len = 0usize;
    let mut stream = response.bytes_stream();
    let stream_request_context = [
        format!("model={model}"),
        "phase=response_stream".to_string(),
    ];

    while !completed {
        let next_chunk = super::io::wait_with_timeout(stream_idle_timeout, stream.next())
            .await
            .map_err(|_| {
                super::ModelRequestFailure::report(
                    super::ModelFailureKind::Timeout,
                    format!(
                        "Responses stream stalled for over {}s (model={}, url={})",
                        stream_idle_timeout.unwrap_or_default().as_secs(),
                        model,
                        url
                    ),
                )
            })?;
        let Some(chunk) = next_chunk else {
            break;
        };
        let chunk = chunk.map_err(|err| {
            format_request_error(
                "Responses stream read failed",
                url,
                &stream_request_context,
                &err,
            )
        })?;
        diagnostics.received(chunk.len());
        buffer.extend_from_slice(&chunk);
        buffer.normalize();

        while let Some(event) = buffer.next_event() {
            let data = event
                .lines()
                .filter_map(|line| line.strip_prefix("data:"))
                .map(str::trim_start)
                .collect::<Vec<_>>()
                .join("\n");
            if data.is_empty() {
                continue;
            }
            if data == "[DONE]" {
                diagnostics.done_marker();
                if !completed {
                    return Err(miette!("Responses stream closed before response.completed"));
                }
                break;
            }
            let value: Value = serde_json::from_str(&data).map_err(|err| {
                diagnostics.invalid_json();
                miette!(
                    "Responses stream event is not valid JSON: {err}; data={}",
                    truncate_for_error(&data)
                )
            })?;
            diagnostics.event(&value);
            match value.get("type").and_then(Value::as_str) {
                Some("response.output_text.delta") => {
                    if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                        delta_content.push_str(delta);
                        assistant_char_len += delta.chars().count();
                        if let Some(progress) = progress {
                            let should_emit = assistant_char_len
                                .saturating_sub(last_assistant_progress_char_len)
                                >= 64
                                || last_assistant_progress_emit_at.elapsed()
                                    >= Duration::from_millis(800);
                            if should_emit && !delta_content.trim().is_empty() {
                                progress.emit_assistant_delta(
                                    &delta_content[last_assistant_progress_byte_len..],
                                );
                                last_assistant_progress_emit_at = Instant::now();
                                last_assistant_progress_char_len = assistant_char_len;
                                last_assistant_progress_byte_len = delta_content.len();
                            }
                        }
                    }
                }
                Some("response.reasoning_summary_text.delta" | "response.reasoning_text.delta") => {
                    if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                        reasoning_content.push_str(delta);
                        reasoning_char_len += delta.chars().count();
                        if let Some(progress) = progress {
                            let should_emit = reasoning_char_len
                                .saturating_sub(last_reasoning_progress_char_len)
                                >= 64
                                || last_reasoning_progress_emit_at.elapsed()
                                    >= Duration::from_millis(800);
                            if should_emit && !reasoning_content.trim().is_empty() {
                                progress.emit_reasoning_delta(
                                    &reasoning_content[last_reasoning_progress_byte_len..],
                                );
                                last_reasoning_progress_emit_at = Instant::now();
                                last_reasoning_progress_char_len = reasoning_char_len;
                                last_reasoning_progress_byte_len = reasoning_content.len();
                            }
                        }
                    }
                }
                Some("response.output_item.done") => {
                    if let Some(item) = value.get("item") {
                        if replayable_output_item(item) {
                            responses_output.push(item.clone());
                        }
                        if let Some(message) = response_item_message_text(item) {
                            output_messages.push(message);
                        }
                        if let Some(reasoning) = response_item_reasoning_text(item) {
                            reasoning_item_content.push_str(&reasoning);
                        }
                        if let Some(call) = response_item_tool_call(item)? {
                            tool_calls.push(call);
                        }
                    }
                }
                Some("response.completed") => {
                    completed = true;
                    response_end_turn = value["response"]["end_turn"].as_bool();
                    if let Some(output) = value["response"]["output"].as_array()
                        && !output.is_empty()
                    {
                        responses_output.clear();
                        output_messages.clear();
                        reasoning_item_content.clear();
                        tool_calls.clear();
                        for item in output {
                            if replayable_output_item(item) {
                                responses_output.push(item.clone());
                            }
                            if let Some(message) = response_item_message_text(item) {
                                output_messages.push(message);
                            }
                            if let Some(reasoning) = response_item_reasoning_text(item) {
                                reasoning_item_content.push_str(&reasoning);
                            }
                            if let Some(call) = response_item_tool_call(item)? {
                                tool_calls.push(call);
                            }
                        }
                    }
                    if let Some(usage) = value
                        .get("response")
                        .and_then(|response| response.get("usage"))
                        .and_then(parse_responses_usage)
                    {
                        record_usage(usage);
                    }
                }
                Some("error" | "response.failed" | "response.incomplete") => {
                    return Err(miette!(
                        "Responses stream failed: {}",
                        truncate_for_json_error(&value)
                    ));
                }
                _ => {}
            }
        }
    }

    if !completed {
        return Err(miette!(
            "Responses stream closed before response.completed (model={}, url={})",
            model,
            url
        ));
    }

    if reasoning_content.trim().is_empty() {
        reasoning_content = reasoning_item_content;
        reasoning_char_len = reasoning_content.chars().count();
    }

    if !reasoning_content.trim().is_empty()
        && reasoning_char_len != last_reasoning_progress_char_len
        && let Some(progress) = progress
    {
        progress.emit_reasoning_delta(&reasoning_content[last_reasoning_progress_byte_len..]);
    }
    if !delta_content.trim().is_empty()
        && assistant_char_len != last_assistant_progress_char_len
        && let Some(progress) = progress
    {
        progress.emit_assistant_delta(&delta_content[last_assistant_progress_byte_len..]);
    }

    let content = if output_messages.is_empty() {
        delta_content
    } else {
        output_messages.join("\n\n")
    };
    let assistant_message = non_empty_string(content);
    let mut items = Vec::with_capacity(tool_calls.len() + usize::from(assistant_message.is_some()));
    if let Some(content) = assistant_message.clone() {
        items.push(AgentTurnItem::AssistantMessage { content });
    }
    items.extend(
        tool_calls
            .iter()
            .cloned()
            .map(|call| AgentTurnItem::ToolCall { call }),
    );

    Ok(AgentTurnStreamResult {
        items,
        raw_stream_follow_up: !tool_calls.is_empty()
            || output_needs_follow_up(&responses_output, response_end_turn),
        last_assistant_message: assistant_message,
        last_reasoning_content: non_empty_string(reasoning_content),
        last_reasoning_signature: None,
        responses_output: (!responses_output.is_empty()).then_some(responses_output),
    })
}

pub(super) fn history_messages_to_responses_parts(
    messages: Vec<HistoryMessage>,
    strip_images: bool,
) -> (String, Vec<Value>) {
    let messages = messages
        .into_iter()
        .map(|message| message.message)
        .collect::<Vec<_>>();
    messages_to_responses_parts(&messages, strip_images)
}

fn responses_user_message(content: &AgentContent, strip_images: bool) -> Value {
    if content.is_plain_text() {
        return json!({"type": "message", "role": "user",
            "content": [{"type": "input_text", "text": content.as_text()}]});
    }

    let mut parts = Vec::new();
    if !content.as_text().trim().is_empty() {
        parts.push(json!({
            "type": "input_text",
            "text": content.as_text(),
        }));
    }
    for part in content.parts() {
        match part {
            AgentContentPart::Text { text } => {
                parts.push(json!({
                    "type": "input_text",
                    "text": text,
                }));
            }
            AgentContentPart::Image { .. }
            | AgentContentPart::ResourceImage { .. }
            | AgentContentPart::ImageData { .. } => {
                let label = part.image_label().unwrap_or("image");
                if strip_images {
                    parts.push(json!({
                        "type": "input_text",
                        "text": format!(
                            "[image: {}]",
                            label
                        ),
                    }));
                    continue;
                }
                let Some(url) = image_part_data_url(part) else {
                    parts.push(json!({
                        "type": "input_text",
                        "text": format!(
                            "[image attachment unavailable: {}]",
                            label
                        ),
                    }));
                    continue;
                };
                parts.push(json!({
                    "type": "input_image",
                    "image_url": url,
                }));
            }
        }
    }
    json!({
        "type": "message",
        "role": "user",
        "content": parts,
    })
}

pub(super) fn sanitize_text_format_name(name: &str) -> String {
    let mut out = String::new();
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "response".to_string()
    } else {
        out
    }
}

fn response_item_message_text(item: &Value) -> Option<String> {
    if item.get("type").and_then(Value::as_str) != Some("message") {
        return None;
    }
    let text = item
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flat_map(|content| content.iter())
        .filter_map(|part| match part.get("type").and_then(Value::as_str) {
            Some("output_text" | "input_text") => part.get("text").and_then(Value::as_str),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");
    (!text.trim().is_empty()).then_some(text)
}

fn response_item_reasoning_text(item: &Value) -> Option<String> {
    if item.get("type").and_then(Value::as_str) != Some("reasoning") {
        return None;
    }
    let mut parts = Vec::new();
    if let Some(summary) = item.get("summary").and_then(Value::as_array) {
        for part in summary {
            if let Some(text) = part.get("text").and_then(Value::as_str)
                && !text.trim().is_empty()
            {
                parts.push(text.to_string());
            }
        }
    }
    if let Some(content) = item.get("content").and_then(Value::as_array) {
        for part in content {
            let is_text = matches!(
                part.get("type").and_then(Value::as_str),
                Some("reasoning_text" | "text")
            );
            if is_text
                && let Some(text) = part.get("text").and_then(Value::as_str)
                && !text.trim().is_empty()
            {
                parts.push(text.to_string());
            }
        }
    }
    (!parts.is_empty()).then(|| parts.join("\n"))
}

fn response_item_tool_call(item: &Value) -> Result<Option<AgentToolCall>> {
    match item.get("type").and_then(Value::as_str) {
        Some("function_call") => {
            let id = item
                .get("call_id")
                .and_then(Value::as_str)
                .or_else(|| item.get("id").and_then(Value::as_str))
                .ok_or_else(|| {
                    miette!(
                        "Responses function_call missing call_id; item={}",
                        truncate_for_json_error(item)
                    )
                })?;
            let name = item.get("name").and_then(Value::as_str).ok_or_else(|| {
                miette!(
                    "Responses function_call missing name; item={}",
                    truncate_for_json_error(item)
                )
            })?;
            let arguments_str = item
                .get("arguments")
                .and_then(Value::as_str)
                .unwrap_or("{}");
            let arguments = serde_json::from_str(arguments_str).map_err(|err| {
                miette!(
                    "failed to decode Responses function_call arguments as JSON: {err}; arguments={}",
                    truncate_for_error(arguments_str)
                )
            })?;
            Ok(Some(AgentToolCall {
                id: id.to_string(),
                name: name.to_string(),
                arguments,
            }))
        }
        Some("custom_tool_call") => {
            let id = item
                .get("call_id")
                .and_then(Value::as_str)
                .or_else(|| item.get("id").and_then(Value::as_str))
                .ok_or_else(|| {
                    miette!(
                        "Responses custom_tool_call missing call_id; item={}",
                        truncate_for_json_error(item)
                    )
                })?;
            let name = item.get("name").and_then(Value::as_str).ok_or_else(|| {
                miette!(
                    "Responses custom_tool_call missing name; item={}",
                    truncate_for_json_error(item)
                )
            })?;
            let input = item
                .get("input")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            Ok(Some(AgentToolCall {
                id: id.to_string(),
                name: name.to_string(),
                arguments: Value::String(input),
            }))
        }
        _ => Ok(None),
    }
}

fn parse_responses_usage(usage: &Value) -> Option<TokenUsage> {
    let input_tokens = usage
        .get("input_tokens")
        .or_else(|| usage.get("prompt_tokens"))
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let output_tokens = usage
        .get("output_tokens")
        .or_else(|| usage.get("completion_tokens"))
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let total_tokens = usage
        .get("total_tokens")
        .and_then(Value::as_i64)
        .unwrap_or_else(|| input_tokens + output_tokens);
    let cached_input_tokens = usage
        .get("input_tokens_details")
        .or_else(|| usage.get("prompt_tokens_details"))
        .and_then(|value| value.get("cached_tokens"))
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let reasoning_output_tokens = usage
        .get("output_tokens_details")
        .or_else(|| usage.get("completion_tokens_details"))
        .and_then(|value| value.get("reasoning_tokens"))
        .and_then(Value::as_i64)
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

#[cfg(test)]
pub(super) async fn test_sse_response(events: &[Value], done: bool) -> reqwest::Response {
    let mut body = events
        .iter()
        .map(|event| format!("data: {event}\n\n"))
        .collect::<String>();
    if done {
        body.push_str("data: [DONE]\n\n");
    }
    test_stream_response(body, "text/event-stream").await
}

#[cfg(test)]
pub(super) async fn test_stream_response(body: String, content_type: &str) -> reqwest::Response {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let content_type = content_type.to_owned();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            socket.read_exact(&mut byte).await.unwrap();
            request.push(byte[0]);
            assert!(request.len() <= 4096, "test request header limit exceeded");
        }
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nX-Request-Id: stream-fixture\r\nSet-Cookie: private-session=do-not-log\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        socket.write_all(response.as_bytes()).await.unwrap();
        socket.shutdown().await.unwrap();
    });
    crate::http_client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .get(format!("http://{address}"))
        .send()
        .await
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model_protocol::AgentToolCall;

    #[test]
    fn reasoning_preserves_effort_and_only_requests_enabled_summaries() {
        for effort in [None, Some("none"), Some("max"), Some("ultra")] {
            for summary in [false, true] {
                let expected_summary = summary && effort != Some("none");
                let payload = reasoning_payload(effort, summary);
                assert_eq!(payload.is_some(), effort.is_some() || expected_summary);
                if let Some(payload) = payload {
                    assert_eq!(payload["effort"].as_str(), effort);
                    assert_eq!(
                        payload["summary"].as_str(),
                        expected_summary.then_some("auto")
                    );
                }
            }
        }
    }

    #[test]
    fn appending_history_preserves_the_previous_input_prefix() {
        let mut messages = vec![AgentMessage::system("base"), AgentMessage::user("first")];
        let (instructions, first) = messages_to_responses_parts(&messages, false);
        messages.extend([
            AgentMessage::assistant("answer"),
            AgentMessage::user("next"),
        ]);
        let (next_instructions, next) = messages_to_responses_parts(&messages, false);
        assert_eq!(instructions, next_instructions);
        assert!(next.starts_with(&first));
        assert_eq!(next.last().unwrap()["content"][0]["text"], "next");
    }

    #[test]
    fn commentary_and_partial_answers_continue_until_final_answer() {
        for phase in ["commentary", "partial_answer"] {
            let output = vec![json!({"type": "message", "phase": phase})];
            assert!(output_needs_follow_up(&output, None));
        }
        let output = vec![json!({"type": "message", "phase": "final_answer"})];
        assert!(!output_needs_follow_up(&output, None));
        assert!(output_needs_follow_up(&output, Some(false)));
        assert!(!output_needs_follow_up(&[json!({"type": "message"})], None));
    }

    #[test]
    fn replay_preserves_phase_encrypted_reasoning_and_custom_tool_pairing() {
        let output = vec![
            json!({"type": "reasoning", "id": "rs_1", "summary": [], "encrypted_content": "opaque"}),
            json!({"type": "message", "id": "msg_1", "role": "assistant", "phase": "commentary", "content": [{"type": "output_text", "text": "Editing"}]}),
            json!({"type": "custom_tool_call", "id": "ctc_1", "call_id": "call_1", "name": "patch", "input": "*** patch"}),
        ];
        let messages = vec![
            AgentMessage::assistant_tool_call_protocol_with_signed_reasoning(
                Some("Editing".into()),
                None,
                None,
                vec![AgentToolCall {
                    id: "call_1".into(),
                    name: "patch".into(),
                    arguments: json!("*** patch"),
                }],
            )
            .with_responses_output(Some(output.clone())),
            AgentMessage::tool("call_1", "patch", "applied"),
        ];
        let persisted = serde_json::to_string(&messages).unwrap();
        let restored: Vec<AgentMessage> = serde_json::from_str(&persisted).unwrap();
        let (_, input) = messages_to_responses_parts(&restored, false);
        assert_eq!(&input[..3], &output);
        assert_eq!(
            input[3],
            json!({"type": "custom_tool_call_output", "call_id": "call_1", "output": "applied"})
        );
    }

    #[test]
    fn legacy_custom_history_uses_custom_input_and_output() {
        let messages = vec![
            AgentMessage::assistant_tool_call_protocol_with_reasoning(
                None,
                None,
                vec![
                    AgentToolCall {
                        id: "custom".into(),
                        name: "patch".into(),
                        arguments: json!("raw input"),
                    },
                    AgentToolCall {
                        id: "function".into(),
                        name: "exec".into(),
                        arguments: json!({"cmd": "pwd"}),
                    },
                ],
            ),
            AgentMessage::tool("custom", "patch", "done"),
            AgentMessage::tool("function", "exec", "cwd"),
        ];
        let (_, input) = messages_to_responses_parts(&messages, false);
        assert_eq!(input[0]["type"], "custom_tool_call");
        assert_eq!(input[0]["input"], "raw input");
        assert_eq!(input[1]["type"], "function_call");
        assert_eq!(input[2]["type"], "custom_tool_call_output");
        assert_eq!(input[3]["type"], "function_call_output");
    }

    #[test]
    fn grammar_tools_use_native_formats_or_strict_schema_fallback() {
        let schema = json!({"type": "object", "properties": {"value": {"type": "string"}}, "required": ["value"], "additionalProperties": false});
        let tool = tool_to_responses_tool(AgentToolSpec {
            name: "exec".into(),
            description: "Execute".into(),
            input_spec: AgentToolInputSpec::JsonSchema {
                schema: schema.clone(),
            },
        });
        assert_eq!(tool["parameters"], schema);
        assert_eq!(tool["strict"], true);
        for syntax in ["lark", "regex", "unsupported"] {
            let schema = json!({"type": "object", "properties": {"input": {"type": "string"}}, "required": ["input"], "additionalProperties": false});
            let tool = tool_to_responses_tool(AgentToolSpec {
                name: "patch".into(),
                description: "Patch".into(),
                input_spec: AgentToolInputSpec::FreeformGrammar {
                    syntax: syntax.into(),
                    definition: "grammar".into(),
                    fallback_schema: schema.clone(),
                },
            });
            if syntax == "unsupported" {
                assert_eq!(tool["type"], "function");
                assert_eq!(tool["strict"], true);
                assert_eq!(tool["parameters"], schema);
                assert!(
                    tool["description"]
                        .as_str()
                        .unwrap()
                        .contains("syntax=unsupported\ndefinition=\ngrammar")
                );
            } else {
                assert_eq!(tool["type"], "custom");
                assert_eq!(tool["format"]["type"], "grammar");
                assert_eq!(tool["format"]["syntax"], syntax);
                assert_eq!(tool["format"]["definition"], "grammar");
            }
        }
    }

    async fn parse_test_stream(
        response: reqwest::Response,
        progress: Option<&ModelProgressSink>,
        idle_timeout: Option<Duration>,
    ) -> Result<AgentTurnStreamResult> {
        let mut diagnostics = super::super::stream_diagnostics::StreamDiagnostics::new(
            &response,
            "responses",
            "fixture-model",
            Some("fixture-actor"),
        );
        let result = parse_stream(
            response,
            progress,
            idle_timeout,
            "fixture-model",
            "https://example.test/responses",
            &mut diagnostics,
            |_| {},
        )
        .await;
        diagnostics.finish(result)
    }

    #[test]
    fn responses_item_parsers_extract_text_and_tool_calls() {
        let message = json!({
            "type": "message",
            "role": "assistant",
            "content": [
                {"type": "output_text", "text": "hello "},
                {"type": "output_text", "text": "world"}
            ]
        });
        assert_eq!(
            response_item_message_text(&message).as_deref(),
            Some("hello world")
        );

        let call = json!({
            "type": "function_call",
            "call_id": "call-2",
            "name": "send_message",
            "arguments": "{\"reply_message\":\"ok\"}"
        });
        let parsed = response_item_tool_call(&call).unwrap().unwrap();
        assert_eq!(parsed.id, "call-2");
        assert_eq!(parsed.name, "send_message");
        assert_eq!(parsed.arguments["reply_message"], "ok");

        let custom = json!({
            "type": "custom_tool_call",
            "call_id": "call-3",
            "name": "custom_patch",
            "input": "--- a\n+++ b\n"
        });
        let parsed = response_item_tool_call(&custom).unwrap().unwrap();
        assert_eq!(
            parsed.arguments,
            Value::String("--- a\n+++ b\n".to_string())
        );
    }

    #[test]
    fn responses_item_parser_extracts_reasoning_summary_and_content() {
        let reasoning = json!({
            "type": "reasoning",
            "id": "rsn-1",
            "summary": [{"type": "summary_text", "text": "considered options"}],
            "content": [{"type": "reasoning_text", "text": "raw thinking"}],
        });
        assert_eq!(
            response_item_reasoning_text(&reasoning).as_deref(),
            Some("considered options\nraw thinking")
        );

        let message = json!({
            "type": "message",
            "content": [{"type": "output_text", "text": "hi"}],
        });
        assert_eq!(response_item_reasoning_text(&message), None);

        let empty = json!({"type": "reasoning", "summary": []});
        assert_eq!(response_item_reasoning_text(&empty), None);
    }

    #[test]
    fn responses_usage_parser_accepts_responses_and_chat_shapes() {
        let usage = parse_responses_usage(&json!({
            "input_tokens": 10,
            "output_tokens": 5,
            "total_tokens": 15,
            "input_tokens_details": {"cached_tokens": 3},
            "output_tokens_details": {"reasoning_tokens": 2}
        }))
        .unwrap();

        assert_eq!(usage.input_tokens, 10);
        assert_eq!(usage.cached_input_tokens, 3);
        assert_eq!(usage.output_tokens, 5);
        assert_eq!(usage.reasoning_output_tokens, 2);
        assert_eq!(usage.total_tokens, 15);
    }

    #[tokio::test]
    async fn responses_progress_emits_only_unsent_utf8_text_and_flushes_the_tail() {
        use crate::core::ModelProgressEvent;
        for tail in [" 尾\n", ""] {
            let assistant = "段🙂".repeat(32);
            let reasoning = "思🙂".repeat(32);
            let mut events = vec![];
            for (text, thinking) in [assistant.as_str(), assistant.as_str(), tail]
                .into_iter()
                .zip([reasoning.as_str(), reasoning.as_str(), tail])
            {
                events.push(json!({"type":"response.output_text.delta","delta":text}));
                events.push(json!({"type":"response.reasoning_text.delta","delta":thinking}));
            }
            events.push(json!({"type":"response.completed","response":{"output":[]}}));
            let response = test_sse_response(&events, true).await;
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let progress = ModelProgressSink::new(tx);
            parse_test_stream(response, Some(&progress), None)
                .await
                .unwrap();
            let mut actual_assistant = vec![];
            let mut actual_reasoning = vec![];
            while let Ok(event) = rx.try_recv() {
                match event {
                    ModelProgressEvent::AssistantDelta { text } => actual_assistant.push(text),
                    ModelProgressEvent::ReasoningDelta { text } => actual_reasoning.push(text),
                    other => panic!("unexpected progress: {other:?}"),
                }
            }
            for (actual, chunk) in [(actual_assistant, assistant), (actual_reasoning, reasoning)] {
                assert_eq!(actual.concat(), format!("{chunk}{chunk}{tail}"));
            }
        }
    }

    #[tokio::test]
    async fn responses_stream_preserves_final_output_and_rejects_missing_completion() {
        let output = json!([
            {"type": "reasoning", "id": "rs_1", "summary": [], "encrypted_content": "opaque"},
            {"type": "message", "id": "msg_1", "role": "assistant", "phase": "commentary", "content": [{"type": "output_text", "text": "Checking"}]},
            {"type": "custom_tool_call", "id": "ctc_1", "call_id": "call_1", "name": "patch", "input": "raw patch"}
        ]);
        // A completion containing output must work even without output_item.done.
        let response = test_sse_response(
            &[
                json!({"type": "response.output_text.delta", "delta": "Checking"}),
                json!({"type": "response.output_item.done", "item": output[0]}),
                json!({"type": "response.completed", "response": {"output": output,
                "usage": {"input_tokens": 100, "output_tokens": 10, "total_tokens": 110}}}),
            ],
            true,
        )
        .await;
        let mut usage = None;
        let mut diagnostics = super::super::stream_diagnostics::StreamDiagnostics::new(
            &response,
            "responses",
            "fixture-model",
            None,
        );
        let result = parse_stream(
            response,
            None,
            None,
            "fixture-model",
            "https://example.test/responses",
            &mut diagnostics,
            |value| usage = Some(value),
        )
        .await;
        let result = diagnostics.finish(result).unwrap();
        assert_eq!(
            usage.unwrap(),
            TokenUsage {
                input_tokens: 100,
                output_tokens: 10,
                total_tokens: 110,
                ..TokenUsage::default()
            }
        );
        assert_eq!(
            result.responses_output,
            Some(output.as_array().unwrap().clone())
        );
        assert_eq!(result.last_assistant_message.as_deref(), Some("Checking"));
        assert!(result.raw_stream_follow_up);
        let protocol = result.protocol();
        assert_eq!(protocol.tool_calls[0].arguments, json!("raw patch"));
        assert_eq!(
            protocol
                .responses_output_for_tool_call("call_1", true)
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            protocol
                .responses_output_for_tool_call("call_1", false)
                .unwrap()
                .len(),
            1
        );

        let response = test_sse_response(&[
            json!({"type":"response.completed","response":{"id":"resp_empty","status":"completed","output":[],"usage":{"output_tokens":0}}}),
        ], false).await;
        let error = parse_test_stream(response, None, None).await.err().unwrap();
        assert!(crate::core::should_retry_agent_turn_error(&error));
        let detail = error.to_string();
        assert!(
            detail.contains("no assistant text")
                && detail.contains("stream-fixture")
                && detail.contains("resp_empty")
        );

        for done in [false, true] {
            let response = test_sse_response(
                &[json!({"type": "response.output_text.delta", "delta": "partial"})],
                done,
            )
            .await;
            let error = parse_test_stream(response, None, None).await.err().unwrap();
            assert!(error.to_string().contains("before response.completed"));
        }
        let response = test_sse_response(
            &[json!({"type": "error", "error": {"message": "backend failure"}})],
            false,
        )
        .await;
        let error = parse_test_stream(response, None, None).await.err().unwrap();
        assert!(error.to_string().contains("backend failure"));
    }

    #[tokio::test(start_paused = true)]
    async fn stream_heartbeats_and_slow_thinking_survive_long_inference_and_disabled_idle_limits() {
        for idle in [Some(Duration::from_secs(900)), None] {
            let events = vec![
                b": keepalive\n\n".to_vec(),
                b"data: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"Thinking\"}\n\n".to_vec(),
                b": keepalive\n\n".to_vec(),
                b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"Done\"}\n\n".to_vec(),
                b"data: {\"type\":\"response.completed\",\"response\":{\"output\":[]}}\n\n".to_vec(),
            ];
            let gap = if idle.is_some() {
                Duration::from_secs(600)
            } else {
                Duration::from_secs(3600)
            };
            let response = super::super::timeout_tests::slow_response(events, gap);
            let result = parse_test_stream(response, None, idle).await.unwrap();
            assert_eq!(result.last_assistant_message.as_deref(), Some("Done"));
            assert_eq!(result.last_reasoning_content.as_deref(), Some("Thinking"));
        }
    }

    #[test]
    fn responses_payload_folds_overlong_tool_call_ids() {
        let long_id = "call:".repeat(20);
        let messages = vec![
            AgentMessage::assistant_tool_call_protocol_with_reasoning(
                None,
                None,
                vec![AgentToolCall {
                    id: long_id.to_string(),
                    name: "exec".to_string(),
                    arguments: json!({"text": ""}),
                }],
            ),
            AgentMessage::tool(long_id, "exec", "ok"),
        ];

        let (_, input) = messages_to_responses_parts(&messages, false);

        let call_id = input[0]["call_id"].as_str().expect("call id");
        assert!(call_id.len() <= 64, "folded call id too long: {call_id}");
        assert_eq!(input[0]["type"], "function_call");
        assert_eq!(input[1]["type"], "function_call_output");
        assert_eq!(input[1]["call_id"], input[0]["call_id"]);
    }

    #[test]
    fn history_keeps_tool_output_before_images_and_can_strip_image_data() {
        use base64::{Engine, engine::general_purpose::STANDARD};
        let data = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.png");
        std::fs::write(&path, STANDARD.decode(data).unwrap()).unwrap();
        for part in [
            AgentContentPart::ImageData {
                media_type: "image/png".into(),
                data_base64: data.into(),
                description: Some("diagram".into()),
            },
            AgentContentPart::Image {
                view: None,
                path: path.display().to_string(),
                media_type: "application/octet-stream".into(),
                description: Some("diagram".into()),
            },
        ] {
            let messages = vec![
                AgentMessage::system("instructions"),
                AgentMessage::assistant_tool_call_protocol_with_reasoning(
                    None,
                    None,
                    vec![AgentToolCall {
                        id: "call-image".into(),
                        name: "capture".into(),
                        arguments: json!({}),
                    }],
                ),
                AgentMessage::tool("call-image", "capture", "captured"),
                AgentMessage::user_content(AgentContent::multimodal("Look", vec![part])),
            ];
            for strip in [false, true] {
                let (instructions, input) = messages_to_responses_parts(&messages, strip);
                assert_eq!(instructions, "instructions");
                assert_eq!(input[0]["type"], "function_call");
                assert_eq!(
                    input[1],
                    json!({"type": "function_call_output", "call_id": "call-image", "output": "captured"})
                );
                assert_eq!(input[2]["role"], "user");
                assert_eq!(
                    input[2]["content"][0],
                    json!({"type": "input_text", "text": "Look"})
                );
                let image = &input[2]["content"][1];
                if strip {
                    assert_eq!(
                        image,
                        &json!({"type": "input_text", "text": "[image: diagram]"})
                    );
                } else {
                    assert_eq!(image["type"], "input_image");
                    let encoded = image["image_url"]
                        .as_str()
                        .unwrap()
                        .strip_prefix("data:image/png;base64,")
                        .unwrap();
                    let decoded =
                        image::load_from_memory(&STANDARD.decode(encoded).unwrap()).unwrap();
                    assert_eq!((decoded.width(), decoded.height()), (1, 1));
                }
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn silent_stream_fails_with_a_typed_idle_timeout() {
        let response = super::super::timeout_tests::slow_response(
            vec![b"data: {}\n\n".to_vec()],
            Duration::from_secs(3600),
        );
        let error = parse_test_stream(response, None, Some(Duration::from_secs(900)))
            .await
            .err()
            .unwrap();
        assert_eq!(
            error
                .downcast_ref::<super::super::ModelRequestFailure>()
                .unwrap()
                .kind,
            super::super::ModelFailureKind::Timeout
        );
    }
}
