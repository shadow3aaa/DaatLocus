use super::transport::{Authentication, BackendHttp};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures_util::StreamExt;
use miette::{Result, miette};
use serde_json::{Value, json};
use tracing::warn;

use super::extract_json_value_from_content;
use super::io::{
    format_request_error, looks_like_context_window_error, non_empty_string,
    read_response_text_with_timeout, truncate_for_error, truncate_for_json_error,
};
use super::payload::image_part_data_url;
use crate::context_budget::{ContextBudgetExceededError, RequestBudgetLimits};
use crate::core::{
    ModelProgressSink, ModelProvider, ModelRequestOptions, TokenUsage, TokenUsageInfo,
};
use crate::model_catalog::catalog_model_capacity;
use crate::model_protocol::{
    AgentContent, AgentContentPart, AgentMessage, AgentToolCall, AgentTurnItem, AgentTurnRequest,
    AgentTurnStreamResult, HistoryMessage, PromptRequest,
};

pub struct ResponsesCompatibleClient {
    transport: BackendHttp,
    api_key: String,
    /// Extra headers attached to each request (e.g. the OpenCode org id).
    extra_headers: reqwest::header::HeaderMap,
    base_url: String,
    model: String,
    thinking_budget: Option<String>,
    request_timeout: Duration,
    stream_idle_timeout: Duration,
    context_window_tokens: usize,
    effective_context_window_tokens: usize,
    auto_compact_threshold_tokens: usize,
    reserved_output_tokens: usize,
    token_usage: std::sync::Mutex<TokenUsageInfo>,
    supports_vision: std::sync::atomic::AtomicBool,
    /// Whether the provider accepts `reasoning.summary`. It is an optional
    /// display enhancement, so runtime errors disable it and retry.
    supports_reasoning_summary: std::sync::atomic::AtomicBool,
}

impl ResponsesCompatibleClient {
    /// Replace the bearer credential used for subsequent requests.
    pub(crate) fn set_api_key(&mut self, api_key: String) {
        self.api_key = api_key;
    }

    /// Install (or replace) one extra header sent with every request.
    pub(crate) fn set_extra_header(&mut self, name: &'static str, value: String) {
        if let Ok(value) = reqwest::header::HeaderValue::from_str(&value) {
            self.extra_headers.insert(name, value);
        }
    }

    pub(crate) fn new(
        api_key: &str,
        base_url: &str,
        model_config: &crate::config::ModelConfig,
    ) -> Self {
        let base_url = crate::config::normalize_provider_base_url(base_url);
        let request_timeout = Duration::from_secs(model_config.request_timeout_secs());
        let stream_idle_timeout = Duration::from_secs(model_config.stream_idle_timeout_secs());
        let transport = BackendHttp::new(&base_url, model_config);
        let context_window_tokens = model_config.context_window_tokens();
        let effective_context_window_tokens = model_config.effective_context_window_tokens();
        let auto_compact_threshold_tokens = model_config.auto_compact_token_limit();
        let reserved_output_tokens = model_config.reserved_output_tokens();
        let supports_vision = model_config.supports_vision.unwrap_or_else(|| {
            catalog_model_capacity(&model_config.model_id).is_some_and(|c| c.supports_vision)
        });
        Self {
            transport,
            api_key: api_key.to_string(),
            extra_headers: reqwest::header::HeaderMap::new(),
            base_url: base_url.clone(),
            model: model_config.model_id.clone(),
            thinking_budget: model_config
                .thinking_budget()
                .map(|budget| budget.as_str().to_string()),
            request_timeout,
            stream_idle_timeout,
            context_window_tokens,
            effective_context_window_tokens,
            auto_compact_threshold_tokens,
            reserved_output_tokens,
            token_usage: std::sync::Mutex::new(TokenUsageInfo {
                total_token_usage: TokenUsage::default(),
                last_token_usage: TokenUsage::default(),
                model_context_window: i64::try_from(context_window_tokens).ok(),
                daily_token_usage: Vec::new(),
            }),
            supports_vision: std::sync::atomic::AtomicBool::new(supports_vision),
            supports_reasoning_summary: std::sync::atomic::AtomicBool::new(true),
        }
    }

    fn url(&self) -> String {
        format!("{}/responses", self.base_url.trim_end_matches('/'))
    }

    const fn request_budget_limits(&self) -> RequestBudgetLimits {
        RequestBudgetLimits {
            context_window_tokens: self.effective_context_window_tokens,
            auto_compact_threshold_tokens: self.auto_compact_threshold_tokens,
            reserved_output_tokens: self.reserved_output_tokens,
        }
    }

    fn record_last_usage(&self, usage: TokenUsage) {
        if let Ok(mut info) = self.token_usage.lock() {
            info.model_context_window = i64::try_from(self.context_window_tokens).ok();
            info.append_last_usage(usage);
        }
    }

    async fn parse_responses_stream(
        &self,
        progress: Option<&ModelProgressSink>,
        response: reqwest::Response,
        emit_progress: bool,
    ) -> Result<AgentTurnStreamResult> {
        let url = self.url();
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
        let mut last_reasoning_progress_emit_at = Instant::now();
        let mut reasoning_char_len = 0usize;
        let mut last_reasoning_progress_char_len = 0usize;
        let mut stream = response.bytes_stream();
        let stream_request_context = [
            format!("model={}", self.model),
            "phase=response_stream".to_string(),
        ];

        while !completed {
            let next_chunk = tokio::time::timeout(self.stream_idle_timeout, stream.next())
                .await
                .map_err(|_| {
                    miette!(
                        "responses-compatible stream stalled for over {}s (model={}, url={})",
                        self.stream_idle_timeout.as_secs(),
                        self.model,
                        url
                    )
                })?;
            let Some(chunk) = next_chunk else {
                break;
            };
            let chunk = chunk.map_err(|err| {
                format_request_error(
                    "responses-compatible stream read failed",
                    &url,
                    &stream_request_context,
                    &err,
                )
            })?;
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
                    break;
                }
                let value: Value = serde_json::from_str(&data).map_err(|err| {
                    miette!(
                        "responses-compatible stream event is not valid JSON: {err}; data={}",
                        truncate_for_error(&data)
                    )
                })?;
                match responses_event_kind(&value) {
                    Some("response.output_text.delta") => {
                        if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                            delta_content.push_str(delta);
                            assistant_char_len += delta.chars().count();
                            if emit_progress {
                                let should_emit = assistant_char_len
                                    .saturating_sub(last_assistant_progress_char_len)
                                    >= 64
                                    || last_assistant_progress_emit_at.elapsed()
                                        >= Duration::from_millis(800);
                                if should_emit && !delta_content.trim().is_empty() {
                                    if let Some(progress) = progress {
                                        progress.emit_assistant_content(delta_content.clone());
                                    }
                                    last_assistant_progress_emit_at = Instant::now();
                                    last_assistant_progress_char_len = assistant_char_len;
                                }
                            }
                        }
                    }
                    Some(
                        "response.reasoning_summary_text.delta" | "response.reasoning_text.delta",
                    ) => {
                        if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                            reasoning_content.push_str(delta);
                            reasoning_char_len += delta.chars().count();
                            if emit_progress {
                                let should_emit = reasoning_char_len
                                    .saturating_sub(last_reasoning_progress_char_len)
                                    >= 64
                                    || last_reasoning_progress_emit_at.elapsed()
                                        >= Duration::from_millis(800);
                                if should_emit && !reasoning_content.trim().is_empty() {
                                    if let Some(progress) = progress {
                                        progress.emit_reasoning_content(reasoning_content.clone());
                                    }
                                    last_reasoning_progress_emit_at = Instant::now();
                                    last_reasoning_progress_char_len = reasoning_char_len;
                                }
                            }
                        }
                    }
                    Some("response.output_item.done") => {
                        if let Some(item) = value.get("item") {
                            if super::responses_protocol::replayable_output_item(item) {
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
                                if super::responses_protocol::replayable_output_item(item) {
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
                            self.record_last_usage(usage);
                        }
                    }
                    Some("error" | "response.failed" | "response.incomplete") => {
                        return Err(miette!(
                            "responses-compatible stream failed: {}",
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
                self.model,
                url
            ));
        }

        if reasoning_content.trim().is_empty() {
            reasoning_content = reasoning_item_content;
            reasoning_char_len = reasoning_content.chars().count();
        }

        if emit_progress
            && !reasoning_content.trim().is_empty()
            && reasoning_char_len != last_reasoning_progress_char_len
            && let Some(progress) = progress
        {
            progress.emit_reasoning_content(reasoning_content.clone());
        }
        if emit_progress
            && !delta_content.trim().is_empty()
            && assistant_char_len != last_assistant_progress_char_len
            && let Some(progress) = progress
        {
            progress.emit_assistant_content(delta_content.clone());
        }

        let content = if output_messages.is_empty() {
            delta_content
        } else {
            output_messages.join("\n\n")
        };
        let assistant_message = non_empty_string(content);
        let mut items =
            Vec::with_capacity(tool_calls.len() + usize::from(assistant_message.is_some()));
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
                || super::responses_protocol::output_needs_follow_up(
                    &responses_output,
                    response_end_turn,
                ),
            last_assistant_message: assistant_message,
            last_reasoning_content: non_empty_string(reasoning_content),
            last_reasoning_signature: None,
            responses_output: (!responses_output.is_empty()).then_some(responses_output),
        })
    }
}

#[async_trait]
impl ModelProvider for ResponsesCompatibleClient {
    async fn complete_json(
        &self,
        request: PromptRequest,
        options: ModelRequestOptions,
    ) -> Result<Value> {
        let budget = &options.budget;
        let request_context = super::io::summarize_prompt_request(&request, Some(budget));
        let backend_request = self.transport.request(
            "/responses",
            Authentication::Bearer(&self.api_key),
            &self.extra_headers,
            &options,
        )?;
        let (instructions, input) =
            history_messages_to_responses_parts(request.all_messages(), false);
        let mut payload = base_payload(
            self,
            &instructions,
            &input,
            &Vec::new(),
            responses_reasoning_payload(self, "prompt"),
        );
        payload["text"] = json!({
            "format": {
                "type": "json_schema",
                "name": sanitize_text_format_name(&request.tool_name),
                "strict": true,
                "schema": request.output_schema.clone(),
            }
        });
        let response = backend_request
            .post(&payload, &request_context, self.request_timeout)
            .await?;
        let status = response.status();
        if !status.is_success() {
            let url = self.url();
            let body = read_response_text_with_timeout(
                response,
                self.request_timeout,
                "responses-compatible body read failed",
                &url,
                &request_context,
            )
            .await?;
            return Err(miette!(
                "responses-compatible returned HTTP {}: {}",
                status,
                truncate_for_error(&body)
            ));
        }
        let result = self
            .parse_responses_stream(options.progress.as_ref(), response, false)
            .await?;
        let content = result.last_assistant_message.as_deref().unwrap_or_default();
        if let Some(value) = extract_json_value_from_content(content) {
            return Ok(value);
        }
        Err(miette!(
            "responses-compatible JSON request did not return a JSON object; content={}",
            truncate_for_error(content)
        ))
    }

    async fn complete_agent_turn(
        &self,
        request: AgentTurnRequest,
        options: ModelRequestOptions,
    ) -> Result<AgentTurnStreamResult> {
        let budget = &options.budget;
        let request_context = super::io::summarize_agent_turn_request(&request, Some(budget));
        let backend_request = self.transport.request(
            "/responses",
            Authentication::Bearer(&self.api_key),
            &self.extra_headers,
            &options,
        )?;
        let mut strip_images = !self.supports_vision.load(Ordering::Relaxed);
        loop {
            let payload = build_agent_payload(self, &request, strip_images);
            let response = backend_request
                .post(&payload, &request_context, self.request_timeout)
                .await?;
            let status = response.status();
            if status.is_success() {
                return self
                    .parse_responses_stream(options.progress.as_ref(), response, true)
                    .await;
            }
            let url = self.url();
            let body = read_response_text_with_timeout(
                response,
                self.request_timeout,
                "responses-compatible body read failed",
                &url,
                &request_context,
            )
            .await?;
            if looks_like_context_window_error(&body) {
                return Err(ContextBudgetExceededError::for_request(
                    "agent turn",
                    &self.model,
                    budget,
                    Some(&format!(
                        "provider_status={status}; provider_body={}",
                        truncate_for_error(&body)
                    )),
                )
                .into());
            }
            if super::io::should_retry_request_without_reasoning_summary(&body)
                && self.supports_reasoning_summary.load(Ordering::Relaxed)
            {
                self.supports_reasoning_summary
                    .store(false, Ordering::Relaxed);
                warn!(
                    "responses-compatible rejected reasoning.summary; retrying without reasoning summaries\n{}",
                    request_context.join("\n")
                );
                continue;
            }
            if super::io::looks_like_vision_unsupported_error(&body)
                && self.supports_vision.load(Ordering::Relaxed)
            {
                self.supports_vision.store(false, Ordering::Relaxed);
                strip_images = true;
                warn!(
                    "responses-compatible rejected image input; retrying without images\n{}",
                    request_context.join("\n")
                );
                continue;
            }
            return Err(miette!(
                "responses-compatible returned HTTP {}: {}",
                status,
                truncate_for_error(&body)
            ));
        }
    }

    fn request_budget_limits(&self) -> RequestBudgetLimits {
        self.request_budget_limits()
    }

    fn token_usage_info(&self) -> TokenUsageInfo {
        self.token_usage
            .lock()
            .ok()
            .map(|info| info.clone())
            .unwrap_or_default()
    }

    fn model_name(&self) -> String {
        self.model.clone()
    }
}

// ---------------------------------------------------------------------------
// Payload construction
// ---------------------------------------------------------------------------

fn base_payload(
    client: &ResponsesCompatibleClient,
    instructions: &str,
    input: &[Value],
    tools: &[Value],
    reasoning: Option<Value>,
) -> Value {
    let mut payload = json!({
        "model": client.model,
        "instructions": instructions,
        "input": input,
        "tools": tools,
        "tool_choice": "auto",
        "parallel_tool_calls": true,
        "store": false,
        "stream": true,
        "include": ["reasoning.encrypted_content"],
    });
    if let Some(reasoning) = reasoning {
        payload["reasoning"] = reasoning;
    }
    payload
}

/// Builds the Responses `reasoning` payload for one request.
///
/// The Responses API only returns reasoning summaries when `summary` is
/// requested, so agent turns ask for them unless thinking is disabled or the
/// provider has already rejected the parameter. Prompt requests stay silent
/// and only forward the configured effort.
fn responses_reasoning_payload(
    client: &ResponsesCompatibleClient,
    request_kind: &str,
) -> Option<Value> {
    let summary = request_kind == "agent"
        && client.supports_reasoning_summary.load(Ordering::Relaxed)
        && !client
            .thinking_budget
            .as_deref()
            .is_some_and(super::thinking::thinking_budget_is_none);
    let effort = client.thinking_budget.as_deref();
    if effort.is_none() && !summary {
        return None;
    }

    let mut reasoning = serde_json::Map::new();
    if let Some(effort) = effort {
        reasoning.insert("effort".to_string(), json!(effort));
    }
    if summary {
        reasoning.insert("summary".to_string(), json!("auto"));
    }
    Some(Value::Object(reasoning))
}

fn build_agent_payload(
    client: &ResponsesCompatibleClient,
    request: &AgentTurnRequest,
    strip_images: bool,
) -> Value {
    let (instructions, input) = agent_messages_to_responses_parts(&request.messages, strip_images);
    let tools = request
        .tools
        .iter()
        .cloned()
        .map(agent_tool_to_responses_tool)
        .collect::<Vec<_>>();
    base_payload(
        client,
        &instructions,
        &input,
        &tools,
        responses_reasoning_payload(client, "agent"),
    )
}

fn history_messages_to_responses_parts(
    messages: Vec<HistoryMessage>,
    strip_images: bool,
) -> (String, Vec<Value>) {
    let messages = messages
        .into_iter()
        .map(|message| message.message)
        .collect::<Vec<_>>();
    agent_messages_to_responses_parts(&messages, strip_images)
}

fn agent_messages_to_responses_parts(
    messages: &[AgentMessage],
    strip_images: bool,
) -> (String, Vec<Value>) {
    super::responses_protocol::messages_to_responses_parts(
        messages,
        strip_images,
        responses_user_message,
    )
}

fn responses_message(role: &str, content_type: &str, text: &str) -> Value {
    json!({
        "type": "message",
        "role": role,
        "content": [{
            "type": content_type,
            "text": text,
        }],
    })
}

fn responses_user_message(content: &AgentContent, strip_images: bool) -> Value {
    if content.is_plain_text() {
        return responses_message("user", "input_text", content.as_text());
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

fn agent_tool_to_responses_tool(tool: crate::model_protocol::AgentToolSpec) -> Value {
    super::responses_protocol::tool_to_responses_tool(tool)
}

fn responses_event_kind(value: &Value) -> Option<&str> {
    value.get("type").and_then(Value::as_str)
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
                        "responses-compatible function_call missing call_id; item={}",
                        truncate_for_json_error(item)
                    )
                })?;
            let name = item.get("name").and_then(Value::as_str).ok_or_else(|| {
                miette!(
                    "responses-compatible function_call missing name; item={}",
                    truncate_for_json_error(item)
                )
            })?;
            let arguments_str = item
                .get("arguments")
                .and_then(Value::as_str)
                .unwrap_or("{}");
            let arguments = serde_json::from_str(arguments_str).map_err(|err| {
                miette!(
                    "failed to decode responses-compatible function_call arguments as JSON: {err}; arguments={}",
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
                        "responses-compatible custom_tool_call missing call_id; item={}",
                        truncate_for_json_error(item)
                    )
                })?;
            let name = item.get("name").and_then(Value::as_str).ok_or_else(|| {
                miette!(
                    "responses-compatible custom_tool_call missing name; item={}",
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

fn sanitize_text_format_name(name: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ModelConfig;

    #[test]
    fn responses_compat_payload_omits_max_output_tokens_parameter() {
        let client = ResponsesCompatibleClient::new(
            "test-key",
            "https://example.test/v1",
            &ModelConfig {
                model_id: "gpt-5.5".to_string(),
                provider: "responses-compatible".to_string(),
                max_completion_tokens: 128_000,
                ..ModelConfig::default()
            },
        );

        let payload = base_payload(&client, "instructions", &[], &[], None);

        assert!(payload.get("max_output_tokens").is_none(), "{payload:#}");
    }

    #[test]
    fn responses_compat_agent_payload_requests_reasoning_summary_and_include() {
        let client = ResponsesCompatibleClient::new(
            "test-key",
            "https://example.test/v1",
            &ModelConfig {
                model_id: "gpt-5.5".to_string(),
                provider: "responses-compatible".to_string(),
                ..ModelConfig::default()
            },
        );

        let payload = build_agent_payload(
            &client,
            &AgentTurnRequest {
                messages: vec![AgentMessage::system("base"), AgentMessage::user("work")],
                tools: Vec::new(),
            },
            false,
        );

        assert_eq!(payload["reasoning"]["summary"], "auto");
        assert_eq!(payload["include"], json!(["reasoning.encrypted_content"]));
    }

    #[test]
    fn responses_compat_prompt_payload_omits_reasoning_summary() {
        let client = ResponsesCompatibleClient::new(
            "test-key",
            "https://example.test/v1",
            &ModelConfig {
                model_id: "gpt-5.5".to_string(),
                provider: "responses-compatible".to_string(),
                ..ModelConfig::default()
            },
        );

        let payload = base_payload(
            &client,
            "instructions",
            &[],
            &[],
            responses_reasoning_payload(&client, "prompt"),
        );

        assert!(payload.get("reasoning").is_none(), "{payload:#}");
        assert_eq!(payload["include"], json!(["reasoning.encrypted_content"]));
    }

    #[test]
    fn responses_payload_keeps_tool_output_before_tool_image_attachment() {
        let client = ResponsesCompatibleClient::new(
            "test-key",
            "https://example.test/v1",
            &ModelConfig {
                model_id: "gpt-5.5".to_string(),
                provider: "responses-compatible".to_string(),
                supports_vision: Some(true),
                ..ModelConfig::default()
            },
        );
        let dir = tempfile::tempdir().expect("temporary directory");
        let image_path = dir.path().join("diagram.png");
        std::fs::write(&image_path, b"\x89PNG\r\n\x1a\nfixture").expect("write image");
        let payload = build_agent_payload(
            &client,
            &AgentTurnRequest {
                messages: vec![
                    AgentMessage::assistant_tool_call_protocol_with_reasoning(
                        None,
                        None,
                        vec![AgentToolCall {
                            id: "call-image".to_string(),
                            name: "view_image".to_string(),
                            arguments: json!({"path": "diagram.png"}),
                        }],
                    ),
                    AgentMessage::tool(
                        "call-image",
                        "view_image",
                        "summary=loaded image diagram.png",
                    ),
                    AgentMessage::user_content(AgentContent::multimodal(
                        "The `view_image` tool attached image content for visual inspection.",
                        vec![AgentContentPart::Image {
                            path: image_path.display().to_string(),
                            media_type: "image/png".to_string(),
                            description: Some("diagram".to_string()),
                        }],
                    )),
                ],
                tools: vec![],
            },
            false,
        );

        assert_eq!(payload["input"][0]["type"], "function_call");
        assert_eq!(payload["input"][1]["type"], "function_call_output");
        assert_eq!(payload["input"][1]["call_id"], "call-image");
        assert_eq!(payload["input"][2]["role"], "user");
        assert_eq!(payload["input"][2]["content"][1]["type"], "input_image");
    }
    #[test]
    fn responses_payload_folds_overlong_tool_call_ids() {
        let long_id = "terminal__terminal_write_stdin:0#5488624ad8e84b8482d6f631d3313011";
        assert!(long_id.len() > 64);
        let messages = vec![
            AgentMessage::assistant_tool_call_protocol_with_reasoning(
                None,
                None,
                vec![AgentToolCall {
                    id: long_id.to_string(),
                    name: "terminal__terminal_write_stdin".to_string(),
                    arguments: json!({"text": ""}),
                }],
            ),
            AgentMessage::tool(long_id, "terminal__terminal_write_stdin", "ok"),
        ];

        let (_, input) = agent_messages_to_responses_parts(&messages, false);

        let call_id = input[0]["call_id"].as_str().expect("call id");
        assert!(call_id.len() <= 64, "folded call id too long: {call_id}");
        assert_eq!(input[0]["type"], "function_call");
        assert_eq!(input[1]["type"], "function_call_output");
        assert_eq!(input[1]["call_id"], input[0]["call_id"]);
    }
    #[tokio::test]
    async fn responses_stream_preserves_final_output_and_rejects_missing_completion() {
        let client = ResponsesCompatibleClient::new(
            "test-key",
            "https://example.test/v1",
            &ModelConfig {
                model_id: "gpt-6.1-sol".into(),
                provider: "responses-compatible".into(),
                ..ModelConfig::default()
            },
        );
        let output = json!([
            {"type": "reasoning", "id": "rs_1", "summary": [], "encrypted_content": "opaque"},
            {"type": "message", "id": "msg_1", "role": "assistant", "phase": "commentary", "content": [{"type": "output_text", "text": "Checking"}]},
            {"type": "custom_tool_call", "id": "ctc_1", "call_id": "call_1", "name": "patch", "input": "raw patch"}
        ]);
        // A completion containing output must work even without output_item.done.
        let response = super::super::responses_protocol::test_sse_response(
            &[
                json!({"type": "response.output_text.delta", "delta": "Checking"}),
                json!({"type": "response.output_item.done", "item": output[0]}),
                json!({"type": "response.completed", "response": {"output": output,
                "usage": {"input_tokens": 100, "output_tokens": 10, "total_tokens": 110}}}),
            ],
            true,
        )
        .await;
        let result = client
            .parse_responses_stream(None, response, false)
            .await
            .unwrap();
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

        for done in [false, true] {
            let response = super::super::responses_protocol::test_sse_response(
                &[json!({"type": "response.output_text.delta", "delta": "partial"})],
                done,
            )
            .await;
            let error = client
                .parse_responses_stream(None, response, false)
                .await
                .err()
                .unwrap();
            assert!(error.to_string().contains("before response.completed"));
        }
        let response = super::super::responses_protocol::test_sse_response(
            &[json!({"type": "error", "error": {"message": "backend failure"}})],
            false,
        )
        .await;
        let error = client
            .parse_responses_stream(None, response, false)
            .await
            .err()
            .unwrap();
        assert!(error.to_string().contains("backend failure"));
    }
    #[test]
    fn responses_reasoning_efforts_are_not_rewritten_or_dropped() {
        for effort in ["none", "max", "ultra"] {
            let config = ModelConfig {
                model_id: "gpt-6.1-sol".into(),
                provider: "test".into(),
                thinking_budget: Some(crate::config::ThinkingBudget::new(effort)),
                ..ModelConfig::default()
            };
            let client = ResponsesCompatibleClient::new("test", "https://example.test/v1", &config);
            let payload = base_payload(
                &client,
                "base",
                &[],
                &[],
                responses_reasoning_payload(&client, "agent"),
            );
            assert_eq!(payload["reasoning"]["effort"], effort);
            if effort == "none" {
                assert!(payload["reasoning"].get("summary").is_none());
            }
        }
    }
}
