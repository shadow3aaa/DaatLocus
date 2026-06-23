use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use miette::{Result, miette};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tracing::warn;

use super::io::{
    default_rate_limit_backoff, format_request_error, looks_like_context_window_error,
    non_empty_string, normalize_sse_buffer, parse_retry_after_seconds,
    read_response_text_with_timeout, send_request_for_streaming_response, take_next_sse_event,
    truncate_for_error, truncate_for_json_error,
};
use super::payload::{flatten_tool_result_as_assistant_text, image_part_data_url};
use super::{extract_json_value_from_content, shared_request_rate_limiter};
use crate::context::Context;
use crate::context_budget::{
    ContextBudgetExceededError, RequestBudgetLimits, estimate_agent_turn_request,
    estimate_prompt_request,
};
use crate::core::{Llm, TokenUsage, TokenUsageInfo};
use crate::model_catalog::catalog_model_capacity;
use crate::reasoning::runtime::{
    AgentContent, AgentContentPart, AgentMessage, AgentToolCall, AgentToolInputSpec, AgentToolSpec,
    AgentTurnItem, AgentTurnRequest, AgentTurnStreamResult, HistoryMessage, PromptRequest,
};

pub(crate) struct ResponsesCompatibleClient {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
    model: String,
    thinking_budget: Option<String>,
    rpm: Option<usize>,
    request_timeout: Duration,
    stream_idle_timeout: Duration,
    context_window_tokens: usize,
    effective_context_window_tokens: usize,
    auto_compact_threshold_tokens: usize,
    reserved_output_tokens: usize,
    request_rate_limiter: Option<Arc<Mutex<VecDeque<Instant>>>>,
    token_usage: std::sync::Mutex<TokenUsageInfo>,
    supports_vision: std::sync::atomic::AtomicBool,
}

impl ResponsesCompatibleClient {
    pub(crate) fn new(
        api_key: &str,
        base_url: &str,
        model_config: &crate::config::ModelConfig,
    ) -> Self {
        let base_url = crate::config::normalize_provider_base_url(base_url);
        let request_timeout = Duration::from_secs(model_config.request_timeout_secs());
        let stream_idle_timeout = Duration::from_secs(model_config.stream_idle_timeout_secs());
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .build()
            .expect("failed to build responses-compatible http client");
        let context_window_tokens = model_config.context_window_tokens();
        let effective_context_window_tokens = model_config.effective_context_window_tokens();
        let auto_compact_threshold_tokens = model_config.auto_compact_token_limit();
        let reserved_output_tokens = model_config.reserved_output_tokens();
        let supports_vision = match model_config.supports_vision {
            Some(v) => v,
            None => catalog_model_capacity(&model_config.model_id)
                .map(|c| c.supports_vision)
                .unwrap_or(false),
        };
        Self {
            client,
            api_key: api_key.to_string(),
            base_url: base_url.clone(),
            model: model_config.model_id.clone(),
            thinking_budget: model_config
                .thinking_budget()
                .map(|budget| budget.as_str().to_string()),
            rpm: model_config.rpm(),
            request_timeout,
            stream_idle_timeout,
            context_window_tokens,
            effective_context_window_tokens,
            auto_compact_threshold_tokens,
            reserved_output_tokens,
            request_rate_limiter: shared_request_rate_limiter(
                &base_url,
                &model_config.model_id,
                model_config.rpm(),
            ),
            token_usage: std::sync::Mutex::new(TokenUsageInfo {
                total_token_usage: TokenUsage::default(),
                last_token_usage: TokenUsage::default(),
                model_context_window: Some(context_window_tokens as i64),
                daily_token_usage: Vec::new(),
            }),
            supports_vision: std::sync::atomic::AtomicBool::new(supports_vision),
        }
    }

    fn url(&self) -> String {
        format!("{}/responses", self.base_url.trim_end_matches('/'))
    }

    fn request_budget_limits(&self) -> RequestBudgetLimits {
        RequestBudgetLimits {
            context_window_tokens: self.effective_context_window_tokens,
            auto_compact_threshold_tokens: self.auto_compact_threshold_tokens,
            reserved_output_tokens: self.reserved_output_tokens,
        }
    }

    async fn wait_for_request_slot(&self, _request_context: &[String]) {
        let Some(limiter) = &self.request_rate_limiter else {
            return;
        };
        let Some(rpm) = self.rpm else {
            return;
        };
        let window = Duration::from_secs(60);
        loop {
            let mut queue = limiter.lock().await;
            let now = Instant::now();
            queue.retain(|t| now.duration_since(*t) < window);
            if queue.len() < rpm {
                queue.push_back(now);
                return;
            }
            let oldest = *queue.front().unwrap();
            let wait = window - now.duration_since(oldest);
            drop(queue);
            tokio::time::sleep(wait).await;
        }
    }

    async fn post_responses_with_retry(
        &self,
        payload: &Value,
        request_context: &[String],
    ) -> Result<reqwest::Response> {
        const MAX_429_RETRIES: usize = 4;
        const MAX_5XX_RETRIES: usize = 3;

        let url = self.url();
        let mut rate_limit_attempt = 0usize;
        let mut transient_attempt = 0usize;
        loop {
            self.wait_for_request_slot(request_context).await;
            let request = self
                .client
                .post(&url)
                .bearer_auth(&self.api_key)
                .json(payload);
            let response = send_request_for_streaming_response(
                request,
                self.request_timeout,
                "responses-compatible request failed",
                &url,
                request_context,
            )
            .await?;

            if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
                let retry_after = response
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|value| value.to_str().ok())
                    .and_then(parse_retry_after_seconds);
                let body = read_response_text_with_timeout(
                    response,
                    self.request_timeout,
                    "responses-compatible 429 body read failed",
                    &url,
                    request_context,
                )
                .await?;

                if rate_limit_attempt >= MAX_429_RETRIES {
                    return Err(miette!(
                        "responses-compatible returned HTTP 429 after {} retries: {}",
                        MAX_429_RETRIES,
                        truncate_for_error(&body)
                    ));
                }

                let delay = retry_after
                    .map(Duration::from_secs)
                    .unwrap_or_else(|| default_rate_limit_backoff(rate_limit_attempt));
                warn!(
                    "responses-compatible returned HTTP 429; retrying in {} ms (attempt {}/{})\n{}",
                    delay.as_millis(),
                    rate_limit_attempt + 1,
                    MAX_429_RETRIES,
                    request_context.join("\n")
                );
                tokio::time::sleep(delay).await;
                rate_limit_attempt += 1;
                continue;
            }

            if response.status().is_server_error() {
                let status = response.status();
                let body = read_response_text_with_timeout(
                    response,
                    self.request_timeout,
                    "responses-compatible 5xx body read failed",
                    &url,
                    request_context,
                )
                .await?;

                if transient_attempt >= MAX_5XX_RETRIES {
                    return Err(miette!(
                        "responses-compatible returned HTTP {} after {} retries: {}",
                        status,
                        MAX_5XX_RETRIES,
                        truncate_for_error(&body)
                    ));
                }

                let delay = Duration::from_millis(400 * (1u64 << transient_attempt));
                warn!(
                    "responses-compatible returned HTTP {}; retrying in {} ms (attempt {}/{})\n{}",
                    status,
                    delay.as_millis(),
                    transient_attempt + 1,
                    MAX_5XX_RETRIES,
                    request_context.join("\n")
                );
                tokio::time::sleep(delay).await;
                transient_attempt += 1;
                continue;
            }

            return Ok(response);
        }
    }

    fn record_last_usage(&self, usage: TokenUsage) {
        if let Ok(mut info) = self.token_usage.lock() {
            info.model_context_window = Some(self.context_window_tokens as i64);
            info.append_last_usage(usage);
        }
    }

    async fn parse_responses_stream(
        &self,
        context: Option<&Context>,
        response: reqwest::Response,
        emit_progress: bool,
    ) -> Result<AgentTurnStreamResult> {
        let url = self.url();
        let mut buffer = String::new();
        let mut delta_content = String::new();
        let mut output_messages = Vec::new();
        let mut reasoning_content = String::new();
        let mut tool_calls = Vec::new();
        let mut completed = false;
        let mut last_assistant_progress_emit_at = Instant::now();
        let mut last_assistant_progress_char_len = 0usize;
        let mut last_reasoning_progress_emit_at = Instant::now();
        let mut last_reasoning_progress_char_len = 0usize;
        let mut stream = response.bytes_stream();
        let stream_request_context = [
            format!("model={}", self.model),
            "phase=response_stream".to_string(),
        ];

        use futures_util::StreamExt;

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
            buffer.push_str(&String::from_utf8_lossy(&chunk));
            normalize_sse_buffer(&mut buffer);

            while let Some(event) = take_next_sse_event(&mut buffer) {
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
                    completed = true;
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
                            if emit_progress {
                                let should_emit = delta_content
                                    .chars()
                                    .count()
                                    .saturating_sub(last_assistant_progress_char_len)
                                    >= 64
                                    || last_assistant_progress_emit_at.elapsed()
                                        >= Duration::from_millis(800);
                                if should_emit && !delta_content.trim().is_empty() {
                                    if let Some(context) = context {
                                        context.emit_live_assistant_progress(&delta_content);
                                    }
                                    last_assistant_progress_emit_at = Instant::now();
                                    last_assistant_progress_char_len =
                                        delta_content.chars().count();
                                }
                            }
                        }
                    }
                    Some("response.reasoning_summary_text.delta")
                    | Some("response.reasoning_text.delta") => {
                        if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                            reasoning_content.push_str(delta);
                            if emit_progress {
                                let should_emit = reasoning_content
                                    .chars()
                                    .count()
                                    .saturating_sub(last_reasoning_progress_char_len)
                                    >= 64
                                    || last_reasoning_progress_emit_at.elapsed()
                                        >= Duration::from_millis(800);
                                if should_emit && !reasoning_content.trim().is_empty() {
                                    if let Some(context) = context {
                                        context.emit_live_reasoning_progress(&reasoning_content);
                                    }
                                    last_reasoning_progress_emit_at = Instant::now();
                                    last_reasoning_progress_char_len =
                                        reasoning_content.chars().count();
                                }
                            }
                        }
                    }
                    Some("response.output_item.done") => {
                        if let Some(item) = value.get("item") {
                            if let Some(message) = response_item_message_text(item) {
                                output_messages.push(message);
                            }
                            if let Some(call) = response_item_tool_call(item)? {
                                tool_calls.push(call);
                            }
                        }
                    }
                    Some("response.completed") => {
                        completed = true;
                        if let Some(usage) = value
                            .get("response")
                            .and_then(|response| response.get("usage"))
                            .and_then(parse_responses_usage)
                        {
                            self.record_last_usage(usage);
                        }
                    }
                    Some("response.failed") | Some("response.incomplete") => {
                        return Err(miette!(
                            "responses-compatible stream failed: {}",
                            truncate_for_json_error(&value)
                        ));
                    }
                    _ => {}
                }
            }
        }

        if emit_progress
            && !reasoning_content.trim().is_empty()
            && reasoning_content.chars().count() != last_reasoning_progress_char_len
            && let Some(context) = context
        {
            context.emit_live_reasoning_progress(&reasoning_content);
        }
        if emit_progress
            && !delta_content.trim().is_empty()
            && delta_content.chars().count() != last_assistant_progress_char_len
            && let Some(context) = context
        {
            context.emit_live_assistant_progress(&delta_content);
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
            raw_stream_follow_up: !tool_calls.is_empty(),
            last_assistant_message: assistant_message,
            last_reasoning_content: non_empty_string(reasoning_content),
        })
    }
}

#[async_trait]
impl Llm for ResponsesCompatibleClient {
    async fn run_json(&self, context: &Context, request: PromptRequest) -> Result<Value> {
        let budget = estimate_prompt_request(&request, self.request_budget_limits());
        if !budget.within_context_window() {
            return Err(ContextBudgetExceededError::for_request(
                "prompt request",
                &self.model,
                &budget,
                None,
            )
            .into());
        }
        let request_context = super::io::summarize_prompt_request(&request, Some(&budget));
        let (instructions, input) =
            history_messages_to_responses_parts(request.all_messages(), false);
        let mut payload = base_payload(self, instructions, input, Vec::new());
        payload["text"] = json!({
            "format": {
                "type": "json_schema",
                "name": sanitize_text_format_name(&request.tool_name),
                "strict": true,
                "schema": request.output_schema.clone(),
            }
        });
        let response = self
            .post_responses_with_retry(&payload, &request_context)
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
            .parse_responses_stream(Some(context), response, false)
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

    async fn run_agent_turn(
        &self,
        context: &Context,
        request: AgentTurnRequest,
    ) -> Result<AgentTurnStreamResult> {
        let budget = estimate_agent_turn_request(
            &request.messages,
            &request.tools,
            self.request_budget_limits(),
        )
        .with_calibrated_input_tokens(&context.token_estimate_baseline);
        if !budget.within_context_window() {
            return Err(ContextBudgetExceededError::for_request(
                "agent turn",
                &self.model,
                &budget,
                None,
            )
            .into());
        }
        let request_context = super::io::summarize_agent_turn_request(&request, Some(&budget));
        use std::sync::atomic::Ordering;
        let strip_images = !self.supports_vision.load(Ordering::Relaxed);
        let payload = build_agent_payload(self, request.clone(), strip_images);
        let response = self
            .post_responses_with_retry(&payload, &request_context)
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
            if looks_like_context_window_error(&body) {
                return Err(ContextBudgetExceededError::for_request(
                    "agent turn",
                    &self.model,
                    &budget,
                    Some(&format!(
                        "provider_status={status}; provider_body={}",
                        truncate_for_error(&body)
                    )),
                )
                .into());
            }
            if super::io::looks_like_vision_unsupported_error(&body)
                && self.supports_vision.load(Ordering::Relaxed)
            {
                self.supports_vision.store(false, Ordering::Relaxed);
                warn!(
                    "responses-compatible rejected image input; retrying without images\n{}",
                    request_context.join("\n")
                );
                let payload = build_agent_payload(self, request, true);
                let response = self
                    .post_responses_with_retry(&payload, &request_context)
                    .await?;
                let status = response.status();
                if status.is_success() {
                    return self
                        .parse_responses_stream(Some(context), response, true)
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
                return Err(miette!(
                    "responses-compatible returned HTTP {}: {}",
                    status,
                    truncate_for_error(&body)
                ));
            }
            return Err(miette!(
                "responses-compatible returned HTTP {}: {}",
                status,
                truncate_for_error(&body)
            ));
        }
        self.parse_responses_stream(Some(context), response, true)
            .await
    }

    fn token_usage_info(&self) -> Option<TokenUsageInfo> {
        self.token_usage.lock().ok().map(|info| info.clone())
    }

    fn model_name(&self) -> Option<String> {
        Some(self.model.clone())
    }
}

// ---------------------------------------------------------------------------
// Payload construction
// ---------------------------------------------------------------------------

fn base_payload(
    client: &ResponsesCompatibleClient,
    instructions: String,
    input: Vec<Value>,
    tools: Vec<Value>,
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
    });
    if let Some(budget) = client.thinking_budget.as_deref()
        && !budget.eq_ignore_ascii_case("none")
    {
        payload["reasoning"] = json!({ "effort": budget });
    }
    payload
}

fn build_agent_payload(
    client: &ResponsesCompatibleClient,
    request: AgentTurnRequest,
    strip_images: bool,
) -> Value {
    let (instructions, input) = agent_messages_to_responses_parts(request.messages, strip_images);
    let tools = request
        .tools
        .into_iter()
        .map(agent_tool_to_responses_tool)
        .collect::<Vec<_>>();
    base_payload(client, instructions, input, tools)
}

fn history_messages_to_responses_parts(
    messages: Vec<HistoryMessage>,
    strip_images: bool,
) -> (String, Vec<Value>) {
    let messages = messages
        .into_iter()
        .map(|message| message.message)
        .collect::<Vec<_>>();
    agent_messages_to_responses_parts(messages, strip_images)
}

fn agent_messages_to_responses_parts(
    messages: Vec<AgentMessage>,
    strip_images: bool,
) -> (String, Vec<Value>) {
    let mut instructions = Vec::new();
    let mut input = Vec::new();
    let mut valid_tool_call_ids = HashSet::new();

    for message in messages {
        match message {
            AgentMessage::System { content } => instructions.push(content),
            AgentMessage::User { content } => {
                input.push(responses_user_message(content, strip_images))
            }
            AgentMessage::Assistant { content } => {
                input.push(responses_message("assistant", "output_text", content));
            }
            AgentMessage::AssistantToolCallProtocol { content, calls, .. } => {
                if let Some(content) = content.filter(|content| !content.trim().is_empty()) {
                    input.push(responses_message("assistant", "output_text", content));
                }
                for call in calls {
                    valid_tool_call_ids.insert(call.id.clone());
                    input.push(json!({
                        "type": "function_call",
                        "call_id": call.id,
                        "name": call.name,
                        "arguments": call.arguments.to_string(),
                    }));
                }
            }
            AgentMessage::Tool {
                tool_call_id,
                name,
                content,
            } => {
                if valid_tool_call_ids.contains(&tool_call_id) {
                    input.push(json!({
                        "type": "function_call_output",
                        "call_id": tool_call_id,
                        "output": content,
                    }));
                } else {
                    input.push(responses_message(
                        "assistant",
                        "output_text",
                        flatten_tool_result_as_assistant_text(&name, &content),
                    ));
                }
            }
        }
    }

    (instructions.join("\n\n"), input)
}

fn responses_message(role: &str, content_type: &str, text: String) -> Value {
    json!({
        "type": "message",
        "role": role,
        "content": [{
            "type": content_type,
            "text": text,
        }],
    })
}

fn responses_user_message(content: AgentContent, strip_images: bool) -> Value {
    if content.is_plain_text() {
        return responses_message("user", "input_text", content.as_text().to_string());
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
            AgentContentPart::Image {
                path, description, ..
            } => {
                if strip_images {
                    parts.push(json!({
                        "type": "input_text",
                        "text": format!(
                            "[image: {}]",
                            description.as_deref().unwrap_or(path)
                        ),
                    }));
                    continue;
                }
                let Some(url) = image_part_data_url(part) else {
                    parts.push(json!({
                        "type": "input_text",
                        "text": format!(
                            "[image attachment unavailable: {}]",
                            description.as_deref().unwrap_or(path)
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

fn agent_tool_to_responses_tool(tool: AgentToolSpec) -> Value {
    match tool.input_spec {
        AgentToolInputSpec::JsonSchema { schema } => json!({
            "type": "function",
            "name": tool.name,
            "description": tool.description,
            "parameters": schema,
        }),
        AgentToolInputSpec::FreeformGrammar {
            syntax,
            definition,
            fallback_schema,
        } => {
            // Responses API may support custom grammars; try function fallback for compat.
            json!({
                "type": "function",
                "name": tool.name,
                "description": format!(
                    "{}\n\nThis is a FREEFORM grammar tool. Put the complete tool input in the `input` field.\nsyntax={syntax}\ndefinition=\n{definition}",
                    tool.description
                ),
                "parameters": fallback_schema,
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Stream event helpers
// ---------------------------------------------------------------------------

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
            Some("output_text") | Some("input_text") => part.get("text").and_then(Value::as_str),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");
    (!text.trim().is_empty()).then_some(text)
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

        let payload = base_payload(&client, "instructions".to_string(), vec![], vec![]);

        assert!(payload.get("max_output_tokens").is_none(), "{payload:#}");
    }
}
