//! 本模块实现实际的llm api调用

use std::{
    collections::{HashMap, HashSet, VecDeque},
    error::Error as _,
    sync::{Arc, LazyLock, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use futures_util::StreamExt;
use miette::{Result, miette};
use parking_lot::Mutex as ParkingLotMutex;
use serde_json::json;
use tracing::warn;

use crate::{
    config::{Config, ModelConfig, ProviderConfig},
    context::Context,
    context_budget::{
        ContextBudgetExceededError, RequestBudgetBreakdown, RequestBudgetLimits,
        estimate_agent_turn_request, estimate_prompt_request,
    },
    core::{LLM, TokenUsage, TokenUsageInfo},
    reasoning::runtime::{
        AgentMessage, AgentToolCall, AgentToolInputSpec, AgentTurnItem, AgentTurnRequest,
        AgentTurnStreamResult, PromptRequest, assistant_tool_call_protocol_char_count,
        summarize_assistant_tool_call_protocol,
    },
    schema_utils::normalize_provider_function_schema,
};

pub struct OpenAIClient {
    client: reqwest::Client,
    pub(crate) api_key: String,
    pub(crate) base_url: String,
    /// chat completions 路径，默认 "/v1/chat/completions"
    completions_path: &'static str,
    /// 每次请求附带的额外 headers（用于 Copilot IDE 鉴权等）
    extra_headers: reqwest::header::HeaderMap,
    model: String,
    temperature: f64,
    thinking_budget: Option<String>,
    rpm: Option<usize>,
    stream_idle_timeout: Duration,
    context_window_tokens: usize,
    effective_context_window_tokens: usize,
    auto_compact_threshold_tokens: usize,
    max_completion_tokens: usize,
    request_rate_limiter: Option<Arc<tokio::sync::Mutex<VecDeque<Instant>>>>,
    adapter_state: Mutex<ChatCompletionsAdapterState>,
    token_usage: Mutex<TokenUsageInfo>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PromptToolChoiceMode {
    NamedFunction,
    RequiredString,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ThinkingBudgetMode {
    ReasoningEffortString,
    NestedReasoningObject,
    Unsupported,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ChatCompletionsAdapterState {
    prompt_tool_choice_mode: PromptToolChoiceMode,
    thinking_budget_mode: ThinkingBudgetMode,
}

impl Default for ChatCompletionsAdapterState {
    fn default() -> Self {
        Self {
            prompt_tool_choice_mode: PromptToolChoiceMode::NamedFunction,
            thinking_budget_mode: ThinkingBudgetMode::ReasoningEffortString,
        }
    }
}

static REQUEST_RATE_LIMITERS: LazyLock<
    ParkingLotMutex<HashMap<String, Arc<tokio::sync::Mutex<VecDeque<Instant>>>>>,
> = LazyLock::new(|| ParkingLotMutex::new(HashMap::new()));

trait ChatCompletionsAdapter {
    fn build_prompt_payload(
        &self,
        client: &OpenAIClient,
        request: &PromptRequest,
        output_schema: serde_json::Value,
    ) -> serde_json::Value;

    fn build_agent_turn_payload(
        &self,
        client: &OpenAIClient,
        request: AgentTurnRequest,
        stream: bool,
    ) -> serde_json::Value;
}

struct StandardChatCompletionsAdapter;

struct CompatibleChatCompletionsAdapter {
    state: ChatCompletionsAdapterState,
}

enum ActiveChatCompletionsAdapter {
    Standard(StandardChatCompletionsAdapter),
    Compatible(CompatibleChatCompletionsAdapter),
}

#[derive(Default, Clone)]
struct StreamingToolCallBuilder {
    id: String,
    name: String,
    arguments: String,
}

impl StreamingToolCallBuilder {
    fn apply_delta(&mut self, delta: &serde_json::Value) {
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

    fn try_build(&self) -> Option<AgentToolCall> {
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

impl OpenAIClient {
    /// 从独立的凭据 + ModelConfig 构造。
    pub fn from_parts(api_key: &str, base_url: &str, model_config: &ModelConfig) -> Self {
        let request_timeout = Duration::from_secs(model_config.request_timeout_secs());
        let stream_idle_timeout = Duration::from_secs(model_config.stream_idle_timeout_secs());
        let client = reqwest::Client::builder()
            .timeout(request_timeout)
            .build()
            .expect("failed to build llm http client");
        let context_window_tokens = model_config.context_window_tokens();
        let effective_context_window_tokens = model_config.effective_context_window_tokens();
        let auto_compact_threshold_tokens = model_config.auto_compact_token_limit();
        let max_completion_tokens = model_config.max_completion_tokens();
        Self {
            client,
            api_key: api_key.to_string(),
            base_url: base_url.to_string(),
            completions_path: "/v1/chat/completions",
            extra_headers: reqwest::header::HeaderMap::new(),
            model: model_config.model_id.clone(),
            temperature: model_config.temperature,
            thinking_budget: model_config.thinking_budget(),
            rpm: model_config.rpm(),
            stream_idle_timeout,
            context_window_tokens,
            effective_context_window_tokens,
            auto_compact_threshold_tokens,
            max_completion_tokens,
            request_rate_limiter: shared_request_rate_limiter(base_url, &model_config.model_id, model_config.rpm()),
            adapter_state: Mutex::new(ChatCompletionsAdapterState::default()),
            token_usage: Mutex::new(TokenUsageInfo {
                total_token_usage: TokenUsage::default(),
                last_token_usage: TokenUsage::default(),
                model_context_window: Some(context_window_tokens as i64),
            }),
        }
    }

    fn url(&self) -> String {
        format!(
            "{}{}",
            self.base_url.trim_end_matches('/'),
            self.completions_path
        )
    }

    fn adapter_state_guard(&self) -> ChatCompletionsAdapterState {
        self.adapter_state
            .lock()
            .map(|state| *state)
            .unwrap_or_default()
    }

    fn update_adapter_state(&self, next: ChatCompletionsAdapterState) {
        if let Ok(mut state) = self.adapter_state.lock() {
            *state = next;
        }
    }

    fn current_adapter(&self) -> ActiveChatCompletionsAdapter {
        if is_standard_openai_base_url(&self.base_url) {
            ActiveChatCompletionsAdapter::Standard(StandardChatCompletionsAdapter)
        } else {
            ActiveChatCompletionsAdapter::Compatible(CompatibleChatCompletionsAdapter {
                state: self.adapter_state_guard(),
            })
        }
    }

    fn request_budget_limits(&self) -> RequestBudgetLimits {
        RequestBudgetLimits {
            context_window_tokens: self.effective_context_window_tokens,
            auto_compact_threshold_tokens: self.auto_compact_threshold_tokens,
            reserved_output_tokens: self.max_completion_tokens,
        }
    }

    async fn wait_for_request_slot(&self, request_context: &[String]) {
        let Some(rpm) = self.rpm else {
            return;
        };
        let Some(limiter) = &self.request_rate_limiter else {
            return;
        };

        let mut logged_wait = false;
        loop {
            let wait_duration = {
                let mut timestamps = limiter.lock().await;
                let now = Instant::now();
                while let Some(front) = timestamps.front().copied() {
                    if now.duration_since(front) >= Duration::from_secs(60) {
                        timestamps.pop_front();
                    } else {
                        break;
                    }
                }

                if timestamps.len() < rpm {
                    timestamps.push_back(now);
                    None
                } else {
                    timestamps.front().copied().map(|front| {
                        Duration::from_secs(60).saturating_sub(now.duration_since(front))
                    })
                }
            };

            let Some(delay) = wait_duration else {
                return;
            };

            if !logged_wait {
                warn!(
                    "llm rpm throttle waiting {} ms before next request (rpm={})\n{}",
                    delay.as_millis(),
                    rpm,
                    request_context.join("\n")
                );
                logged_wait = true;
            }
            tokio::time::sleep(delay).await;
        }
    }

    async fn post_json_with_rate_limit_retry(
        &self,
        url: &str,
        payload: &serde_json::Value,
        request_context: &[String],
    ) -> Result<reqwest::Response> {
        const MAX_429_RETRIES: usize = 4;
        const MAX_5XX_RETRIES: usize = 3;

        let mut rate_limit_attempt = 0usize;
        let mut transient_attempt = 0usize;
        loop {
            self.wait_for_request_slot(request_context).await;
            let response = self
                .client
                .post(url)
                .bearer_auth(&self.api_key)
                .headers(self.extra_headers.clone())
                .json(payload)
                .send()
                .await
                .map_err(|err| {
                    format_request_error("llm request failed", url, request_context, &err)
                })?;

            if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
                let retry_after = response
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|value| value.to_str().ok())
                    .and_then(parse_retry_after_seconds);
                let body = response
                    .text()
                    .await
                    .map_err(|err| miette!("llm response body read failed: {err}"))?;

                if rate_limit_attempt >= MAX_429_RETRIES {
                    return Err(miette!(
                        "llm api returned HTTP 429 after {} retries: {}",
                        MAX_429_RETRIES,
                        truncate_for_error(&body)
                    ));
                }

                let delay = retry_after
                    .map(Duration::from_secs)
                    .unwrap_or_else(|| default_rate_limit_backoff(rate_limit_attempt));
                let delay_ms = delay.as_millis();
                warn!(
                    "llm api returned HTTP 429; retrying request in {} ms (attempt {}/{})\n{}",
                    delay_ms,
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
                let body = response
                    .text()
                    .await
                    .map_err(|err| miette!("llm response body read failed: {err}"))?;

                if transient_attempt >= MAX_5XX_RETRIES {
                    return Err(miette!(
                        "llm api returned HTTP {} after {} retries: {}",
                        status,
                        MAX_5XX_RETRIES,
                        truncate_for_error(&body)
                    ));
                }

                let delay = Duration::from_millis(400 * (1u64 << transient_attempt));
                let delay_ms = delay.as_millis();
                warn!(
                    "llm api returned HTTP {}; retrying request in {} ms (attempt {}/{})\n{}",
                    status,
                    delay_ms,
                    transient_attempt + 1,
                    MAX_5XX_RETRIES,
                    request_context.join("\n")
                );
                tokio::time::sleep(delay).await;
                transient_attempt += 1;
                continue;
            }

            {
                return Ok(response);
            }
        }
    }

    async fn call_tool_json(&self, request: PromptRequest) -> Result<serde_json::Value> {
        let url = self.url();
        let output_schema = normalize_provider_function_schema(request.output_schema.clone());
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
        let request_context = summarize_prompt_request(&request, Some(&budget));
        let mut adapter_state = self.adapter_state_guard();
        let body = loop {
            let payload =
                self.current_adapter()
                    .build_prompt_payload(self, &request, output_schema.clone());
            let response = self
                .post_json_with_rate_limit_retry(&url, &payload, &request_context)
                .await?;
            let status = response.status();
            let body = response
                .text()
                .await
                .map_err(|err| miette!("llm response body read failed: {err}"))?;

            if status.is_success() {
                break body;
            }

            if looks_like_context_window_error(&body) {
                return Err(ContextBudgetExceededError::for_request(
                    "prompt request",
                    &self.model,
                    &budget,
                    Some(&format!(
                        "provider_status={status}; provider_body={}",
                        truncate_for_error(&body)
                    )),
                )
                .into());
            }

            if should_retry_prompt_request_with_string_tool_choice(&body)
                && adapter_state.prompt_tool_choice_mode != PromptToolChoiceMode::RequiredString
            {
                adapter_state.prompt_tool_choice_mode = PromptToolChoiceMode::RequiredString;
                self.update_adapter_state(adapter_state);
                warn!(
                    "llm provider rejected named function tool_choice; retrying prompt request with string tool_choice\n{}",
                    request_context.join("\n")
                );
                continue;
            }

            if self.thinking_budget.is_some()
                && should_retry_prompt_request_with_nested_thinking_budget(&body)
                && adapter_state.thinking_budget_mode == ThinkingBudgetMode::ReasoningEffortString
            {
                adapter_state.thinking_budget_mode = ThinkingBudgetMode::NestedReasoningObject;
                self.update_adapter_state(adapter_state);
                warn!(
                    "llm provider rejected reasoning_effort; retrying prompt request with reasoning.effort\n{}",
                    request_context.join("\n")
                );
                continue;
            }

            if self.thinking_budget.is_some()
                && should_retry_request_without_thinking_budget(&body)
                && adapter_state.thinking_budget_mode != ThinkingBudgetMode::Unsupported
            {
                adapter_state.thinking_budget_mode = ThinkingBudgetMode::Unsupported;
                self.update_adapter_state(adapter_state);
                warn!(
                    "llm provider rejected thinking budget parameter; retrying prompt request without it\n{}",
                    request_context.join("\n")
                );
                continue;
            }

            return Err(miette!(
                "llm api returned HTTP {}: {}",
                status,
                truncate_for_error(&body)
            ));
        };

        let response_json: serde_json::Value = serde_json::from_str(&body).map_err(|err| {
            miette!(
                "llm response is not valid JSON: {err}; body={}",
                truncate_for_error(&body)
            )
        })?;
        self.record_usage_from_response(&response_json);
        let content = response_json["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("");
        let Some(tool_calls) = response_json["choices"][0]["message"]["tool_calls"].as_array()
        else {
            if let Some(value) = extract_json_value_from_content(content) {
                return Ok(value);
            }
            return Err(miette!(
                "llm response did not include tool_calls; content={}; response={}",
                truncate_for_error(content),
                truncate_for_json_error(&response_json)
            ));
        };
        let first_tool_call = if let Some(first_tool_call) = tool_calls.first() {
            first_tool_call
        } else if let Some(value) = extract_json_value_from_content(content) {
            return Ok(value);
        } else {
            return Err(miette!(
                "llm response included empty tool_calls; content={}; response={}",
                truncate_for_error(content),
                truncate_for_json_error(&response_json)
            ));
        };
        let arguments_str = first_tool_call["function"]["arguments"]
            .as_str()
            .ok_or_else(|| {
                miette!(
                    "llm response missing function.arguments string; response={}",
                    truncate_for_json_error(&response_json)
                )
            })?;
        serde_json::from_str(arguments_str).map_err(|err| {
            miette!(
                "failed to decode tool arguments as JSON: {err}; arguments={}",
                truncate_for_error(arguments_str)
            )
        })
    }

    async fn call_agent_turn(
        &self,
        context: &Context,
        request: AgentTurnRequest,
    ) -> Result<AgentTurnStreamResult> {
        let url = self.url();
        let budget = estimate_agent_turn_request(
            &request.messages,
            &request.tools,
            self.request_budget_limits(),
        );
        if !budget.within_context_window() {
            return Err(ContextBudgetExceededError::for_request(
                "agent turn",
                &self.model,
                &budget,
                None,
            )
            .into());
        }
        let request_context = summarize_agent_turn_request(&request, Some(&budget));
        let mut adapter_state = self.adapter_state_guard();
        let (response, content_type) = loop {
            let payload =
                self.current_adapter()
                    .build_agent_turn_payload(self, request.clone(), true);
            let response = self
                .post_json_with_rate_limit_retry(&url, &payload, &request_context)
                .await?;
            let status = response.status();
            let content_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string();

            if status.is_success() {
                break (response, content_type);
            }

            let body = response
                .text()
                .await
                .map_err(|err| miette!("llm response body read failed: {err}"))?;
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

            if self.thinking_budget.is_some()
                && should_retry_prompt_request_with_nested_thinking_budget(&body)
                && adapter_state.thinking_budget_mode == ThinkingBudgetMode::ReasoningEffortString
            {
                adapter_state.thinking_budget_mode = ThinkingBudgetMode::NestedReasoningObject;
                self.update_adapter_state(adapter_state);
                warn!(
                    "llm provider rejected reasoning_effort; retrying agent turn with reasoning.effort\n{}",
                    request_context.join("\n")
                );
                continue;
            }

            if self.thinking_budget.is_some()
                && should_retry_request_without_thinking_budget(&body)
                && adapter_state.thinking_budget_mode != ThinkingBudgetMode::Unsupported
            {
                adapter_state.thinking_budget_mode = ThinkingBudgetMode::Unsupported;
                self.update_adapter_state(adapter_state);
                warn!(
                    "llm provider rejected thinking budget parameter; retrying agent turn without it\n{}",
                    request_context.join("\n")
                );
                continue;
            }

            return Err(miette!(
                "llm api returned HTTP {}: {}",
                status,
                truncate_for_error(&body)
            ));
        };

        if !content_type.contains("text/event-stream") {
            let body = response
                .text()
                .await
                .map_err(|err| miette!("llm response body read failed: {err}"))?;
            let response_json: serde_json::Value = serde_json::from_str(&body).map_err(|err| {
                miette!(
                    "llm response is not valid JSON: {err}; body={}",
                    truncate_for_error(&body)
                )
            })?;
            self.record_usage_from_response(&response_json);
            return parse_agent_turn_stream_result_from_json(&response_json);
        }

        let mut buffer = String::new();
        let mut content = String::new();
        let mut tool_calls: Vec<StreamingToolCallBuilder> = Vec::new();
        let mut last_usage = None;
        let mut last_progress_emit_at = Instant::now();
        let mut last_progress_char_len = 0usize;
        let mut stream = response.bytes_stream();
        let mut stream_done = false;
        while !stream_done {
            let next_chunk = tokio::time::timeout(self.stream_idle_timeout, stream.next())
                .await
                .map_err(|_| {
                    miette!(
                        "llm streaming response stalled for over {}s (model={}, url={})",
                        self.stream_idle_timeout.as_secs(),
                        self.model,
                        url
                    )
                })?;
            let Some(chunk) = next_chunk else {
                break;
            };
            let chunk = chunk.map_err(|err| miette!("llm streaming chunk read failed: {err}"))?;
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
                    stream_done = true;
                    break;
                }
                let response_json: serde_json::Value =
                    serde_json::from_str(&data).map_err(|err| {
                        miette!(
                            "llm streaming chunk is not valid JSON: {err}; data={}",
                            truncate_for_error(&data)
                        )
                    })?;
                if let Some(usage) = parse_usage_from_response_json(&response_json) {
                    last_usage = Some(usage);
                }
                let choice = &response_json["choices"][0];
                let delta = &choice["delta"];
                if let Some(delta_content) = delta["content"].as_str() {
                    content.push_str(delta_content);
                    let should_emit = content
                        .chars()
                        .count()
                        .saturating_sub(last_progress_char_len)
                        >= 64
                        || last_progress_emit_at.elapsed() >= Duration::from_millis(800);
                    if should_emit && !content.trim().is_empty() {
                        context.emit_live_assistant_progress(&content);
                        last_progress_emit_at = Instant::now();
                        last_progress_char_len = content.chars().count();
                    }
                }
                if let Some(delta_tool_calls) = delta["tool_calls"].as_array() {
                    for tool_call in delta_tool_calls {
                        let Some(index) = tool_call["index"].as_u64().map(|index| index as usize)
                        else {
                            continue;
                        };
                        while tool_calls.len() <= index {
                            tool_calls.push(StreamingToolCallBuilder::default());
                        }
                        tool_calls[index].apply_delta(tool_call);
                    }
                }
            }
        }
        if !content.trim().is_empty() && content.chars().count() != last_progress_char_len {
            context.emit_live_assistant_progress(&content);
        }
        if let Some(usage) = last_usage {
            self.record_last_usage(usage);
        }

        if !tool_calls.is_empty() {
            let mut calls = Vec::with_capacity(tool_calls.len());
            for (index, builder) in tool_calls.into_iter().enumerate() {
                let call = builder.try_build().ok_or_else(|| {
                    miette!(
                        "llm streaming response ended with incomplete tool call at index {index}"
                    )
                })?;
                calls.push(call);
            }
            let assistant_message = if content.trim().is_empty() {
                None
            } else {
                Some(content)
            };
            let mut items =
                Vec::with_capacity(calls.len() + usize::from(assistant_message.is_some()));
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
        })
    }

    fn record_usage_from_response(&self, response_json: &serde_json::Value) {
        if let Some(usage) = parse_usage_from_response_json(response_json) {
            self.record_last_usage(usage);
        }
    }

    fn record_last_usage(&self, usage: TokenUsage) {
        if let Ok(mut info) = self.token_usage.lock() {
            info.model_context_window = Some(self.context_window_tokens as i64);
            info.append_last_usage(usage);
        }
    }
}

impl ChatCompletionsAdapter for StandardChatCompletionsAdapter {
    fn build_prompt_payload(
        &self,
        client: &OpenAIClient,
        request: &PromptRequest,
        output_schema: serde_json::Value,
    ) -> serde_json::Value {
        let messages = prompt_request_to_openai_messages(request.clone(), false);
        let mut payload = json!({
            "model": client.model,
            "messages": messages,
            "tools": [
                {
                    "type": "function",
                    "function": {
                        "strict": true,
                        "name": request.tool_name,
                        "description": request.tool_description,
                        "parameters": output_schema
                    }
                }
            ],
            "tool_choice": {
                "type": "function",
                "function": { "name": request.tool_name }
            },
            "temperature": client.temperature,
            "max_tokens": client.max_completion_tokens,
        });
        apply_optional_thinking_budget(
            &mut payload,
            client.thinking_budget.as_deref(),
            client.adapter_state_guard().thinking_budget_mode,
        );
        payload
    }

    fn build_agent_turn_payload(
        &self,
        client: &OpenAIClient,
        request: AgentTurnRequest,
        stream: bool,
    ) -> serde_json::Value {
        build_agent_turn_payload_common(client, request, stream, false)
    }
}

impl ChatCompletionsAdapter for CompatibleChatCompletionsAdapter {
    fn build_prompt_payload(
        &self,
        client: &OpenAIClient,
        request: &PromptRequest,
        output_schema: serde_json::Value,
    ) -> serde_json::Value {
        let messages = prompt_request_to_openai_messages(request.clone(), true);
        let tool_choice = match self.state.prompt_tool_choice_mode {
            PromptToolChoiceMode::NamedFunction => {
                json!({
                    "type": "function",
                    "function": { "name": request.tool_name }
                })
            }
            PromptToolChoiceMode::RequiredString => json!("required"),
        };
        let mut payload = json!({
            "model": client.model,
            "messages": messages,
            "tools": [
                {
                    "type": "function",
                    "function": {
                        "strict": true,
                        "name": request.tool_name,
                        "description": request.tool_description,
                        "parameters": output_schema
                    }
                }
            ],
            "tool_choice": tool_choice,
            "temperature": client.temperature,
            "max_tokens": client.max_completion_tokens,
        });
        apply_optional_thinking_budget(
            &mut payload,
            client.thinking_budget.as_deref(),
            self.state.thinking_budget_mode,
        );
        payload
    }

    fn build_agent_turn_payload(
        &self,
        client: &OpenAIClient,
        request: AgentTurnRequest,
        stream: bool,
    ) -> serde_json::Value {
        build_agent_turn_payload_common(client, request, stream, true)
    }
}

impl ChatCompletionsAdapter for ActiveChatCompletionsAdapter {
    fn build_prompt_payload(
        &self,
        client: &OpenAIClient,
        request: &PromptRequest,
        output_schema: serde_json::Value,
    ) -> serde_json::Value {
        match self {
            Self::Standard(adapter) => adapter.build_prompt_payload(client, request, output_schema),
            Self::Compatible(adapter) => {
                adapter.build_prompt_payload(client, request, output_schema)
            }
        }
    }

    fn build_agent_turn_payload(
        &self,
        client: &OpenAIClient,
        request: AgentTurnRequest,
        stream: bool,
    ) -> serde_json::Value {
        match self {
            Self::Standard(adapter) => adapter.build_agent_turn_payload(client, request, stream),
            Self::Compatible(adapter) => adapter.build_agent_turn_payload(client, request, stream),
        }
    }
}

fn is_standard_openai_base_url(base_url: &str) -> bool {
    let normalized = base_url.trim_end_matches('/');
    normalized.contains("api.openai.com") || normalized.contains("chatgpt.com/backend-api/codex")
}

fn should_retry_prompt_request_with_string_tool_choice(body: &str) -> bool {
    let body = body.to_ascii_lowercase();
    body.contains("unknown parameter: 'tool_choice.function'")
        || body.contains("unknown parameter: \"tool_choice.function\"")
}

fn should_retry_prompt_request_with_nested_thinking_budget(body: &str) -> bool {
    let body = body.to_ascii_lowercase();
    body.contains("unknown parameter: 'reasoning_effort'")
        || body.contains("unknown parameter: \"reasoning_effort\"")
}

fn should_retry_request_without_thinking_budget(body: &str) -> bool {
    let body = body.to_ascii_lowercase();
    body.contains("unknown parameter: 'reasoning'")
        || body.contains("unknown parameter: \"reasoning\"")
        || body.contains("unknown parameter: 'reasoning.effort'")
        || body.contains("unknown parameter: \"reasoning.effort\"")
}

fn build_agent_turn_payload_common(
    client: &OpenAIClient,
    request: AgentTurnRequest,
    stream: bool,
    flatten_unmatched_tool_messages: bool,
) -> serde_json::Value {
    let allowed_tool_names = request
        .tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<HashSet<_>>();
    let messages = agent_turn_request_to_openai_messages(
        request.messages,
        flatten_unmatched_tool_messages,
        &allowed_tool_names,
    );
    let tools = request
        .tools
        .into_iter()
        .map(|tool| {
            let (description, parameters, strict) = match tool.input_spec {
                AgentToolInputSpec::JsonSchema { schema } => (
                    tool.description,
                    normalize_provider_function_schema(schema),
                    true,
                ),
                AgentToolInputSpec::FreeformGrammar {
                    syntax,
                    definition,
                    fallback_schema,
                } => (
                    format!(
                        "{}\n\n这是一个 FREEFORM grammar tool。当前 provider 回退为单字符串输入：请把完整工具输入放进 `input` 字段。\nsyntax={syntax}\ndefinition=\n{definition}",
                        tool.description
                    ),
                    normalize_provider_function_schema(fallback_schema),
                    false,
                ),
            };
            json!({
                "type": "function",
                "function": {
                    "strict": strict,
                    "name": tool.name,
                    "description": description,
                    "parameters": parameters,
                }
            })
        })
        .collect::<Vec<_>>();
    let mut payload = json!({
        "model": client.model,
        "messages": messages,
        "tools": tools,
        "temperature": client.temperature,
        "max_tokens": client.max_completion_tokens,
        "stream": stream,
        "stream_options": if stream {
            json!({ "include_usage": true })
        } else {
            serde_json::Value::Null
        },
    });
    apply_optional_thinking_budget(
        &mut payload,
        client.thinking_budget.as_deref(),
        client.adapter_state_guard().thinking_budget_mode,
    );
    payload
}

fn apply_optional_thinking_budget(
    payload: &mut serde_json::Value,
    thinking_budget: Option<&str>,
    mode: ThinkingBudgetMode,
) {
    let Some(thinking_budget) = thinking_budget else {
        return;
    };
    let Some(object) = payload.as_object_mut() else {
        return;
    };
    match mode {
        ThinkingBudgetMode::ReasoningEffortString => {
            object.insert("reasoning_effort".to_string(), json!(thinking_budget));
        }
        ThinkingBudgetMode::NestedReasoningObject => {
            object.insert(
                "reasoning".to_string(),
                json!({ "effort": thinking_budget }),
            );
        }
        ThinkingBudgetMode::Unsupported => {}
    }
}

/// 展开 `${VAR_NAME}` 形式的环境变量引用；若无法展开则返回原值。
fn resolve_env_var_ref(value: &str) -> String {
    let trimmed = value.trim();
    if let Some(inner) = trimmed.strip_prefix("${").and_then(|s| s.strip_suffix('}')) {
        std::env::var(inner).unwrap_or_else(|_| trimmed.to_string())
    } else if let Some(inner) = trimmed.strip_prefix('$') {
        std::env::var(inner).unwrap_or_else(|_| trimmed.to_string())
    } else {
        trimmed.to_string()
    }
}

fn shared_request_rate_limiter(
    base_url: &str,
    model_id: &str,
    rpm: Option<usize>,
) -> Option<Arc<tokio::sync::Mutex<VecDeque<Instant>>>> {
    let rpm = rpm?;
    let key = format!(
        "{}\u{1f}{}\u{1f}{}",
        base_url.trim_end_matches('/'),
        model_id,
        rpm
    );
    let mut registry = REQUEST_RATE_LIMITERS.lock();
    Some(
        registry
            .entry(key)
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(VecDeque::new())))
            .clone(),
    )
}

fn extract_json_value_from_content(content: &str) -> Option<serde_json::Value> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
        return Some(value);
    }
    let fenced = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```JSON"))
        .or_else(|| trimmed.strip_prefix("```"));
    if let Some(fenced) = fenced {
        let fenced = fenced.trim();
        let fenced = fenced.strip_suffix("```").unwrap_or(fenced).trim();
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(fenced) {
            return Some(value);
        }
    }
    None
}

fn summarize_agent_turn_request(
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

fn summarize_prompt_request(
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
        AgentMessage::AssistantToolCallProtocol { content, calls } => {
            assistant_tool_call_protocol_char_count(content.as_deref(), calls)
        }
        AgentMessage::Tool {
            tool_call_id,
            name,
            content,
        } => tool_call_id.chars().count() + name.chars().count() + content.chars().count(),
    }
}

fn parse_agent_turn_stream_result_from_json(
    response_json: &serde_json::Value,
) -> Result<AgentTurnStreamResult> {
    let message = &response_json["choices"][0]["message"];
    let content = message["content"]
        .as_str()
        .map(|text| text.to_string())
        .unwrap_or_default();

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
    })
}

fn parse_usage_from_response_json(response_json: &serde_json::Value) -> Option<TokenUsage> {
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

fn normalize_sse_buffer(buffer: &mut String) {
    if buffer.contains('\r') {
        *buffer = buffer.replace("\r\n", "\n").replace('\r', "\n");
    }
}

fn take_next_sse_event(buffer: &mut String) -> Option<String> {
    let delimiter_index = buffer.find("\n\n")?;
    let event = buffer[..delimiter_index].to_string();
    buffer.drain(..delimiter_index + 2);
    Some(event)
}

fn format_request_error(
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

#[async_trait]
impl LLM for OpenAIClient {
    async fn run_json(
        &self,
        _context: &Context,
        request: PromptRequest,
    ) -> Result<serde_json::Value> {
        self.call_tool_json(request).await
    }

    async fn run_agent_turn(
        &self,
        context: &Context,
        request: AgentTurnRequest,
    ) -> Result<AgentTurnStreamResult> {
        self.call_agent_turn(context, request).await
    }

    fn token_usage_info(&self) -> Option<TokenUsageInfo> {
        self.token_usage.lock().ok().map(|info| info.clone())
    }

    fn model_name(&self) -> Option<String> {
        Some(self.model.clone())
    }
}

fn history_message_to_openai_message(
    message: crate::reasoning::runtime::HistoryMessage,
    flatten_tool_messages: bool,
) -> serde_json::Value {
    provider_message_from_agent_message(&message.message, flatten_tool_messages)
}

fn prompt_request_to_openai_messages(
    request: PromptRequest,
    flatten_tool_messages: bool,
) -> Vec<serde_json::Value> {
    request
        .system_messages
        .into_iter()
        .map(|message| json!({"role": "system", "content": message}))
        .chain(
            request
                .long_term_memory_messages
                .into_iter()
                .map(|message| history_message_to_openai_message(message, flatten_tool_messages)),
        )
        .chain(
            request
                .history_messages
                .into_iter()
                .map(|message| history_message_to_openai_message(message, flatten_tool_messages)),
        )
        .chain(std::iter::once(json!({
            "role": "user",
            "content": request.current_user_message,
        })))
        .chain(
            request
                .retry_messages
                .into_iter()
                .map(|message| history_message_to_openai_message(message, flatten_tool_messages)),
        )
        .collect::<Vec<_>>()
}

fn agent_message_to_openai_message(message: AgentMessage) -> serde_json::Value {
    match message {
        AgentMessage::System { content } => json!({
            "role": "system",
            "content": content,
        }),
        AgentMessage::User { content } => json!({
            "role": "user",
            "content": content,
        }),
        AgentMessage::Assistant { content } => json!({
            "role": "assistant",
            "content": content,
        }),
        AgentMessage::AssistantToolCallProtocol { content, calls } => json!({
            "role": "assistant",
            "content": content.unwrap_or_default(),
            "tool_calls": calls.into_iter().map(|call| json!({
                "id": call.id,
                "type": "function",
                "function": {
                    "name": call.name,
                    "arguments": call.arguments.to_string(),
                }
            })).collect::<Vec<_>>(),
        }),
        AgentMessage::Tool {
            tool_call_id,
            name,
            content,
        } => json!({
            "role": "tool",
            "tool_call_id": tool_call_id,
            "name": name,
            "content": content,
        }),
    }
}

fn provider_message_from_agent_message(
    message: &AgentMessage,
    flatten_non_plain_messages: bool,
) -> serde_json::Value {
    if flatten_non_plain_messages {
        match message {
            AgentMessage::AssistantToolCallProtocol { content, calls } => {
                return json!({
                    "role": "assistant",
                    "content": summarize_assistant_tool_call_protocol(content.as_deref(), calls),
                });
            }
            AgentMessage::Tool { name, content, .. } => {
                return json!({
                    "role": "assistant",
                    "content": flatten_tool_result_as_assistant_text(name, content),
                });
            }
            _ => {}
        }
    }
    agent_message_to_openai_message(message.clone())
}

fn agent_turn_request_to_openai_messages(
    messages: Vec<AgentMessage>,
    flatten_unmatched_tool_messages: bool,
    allowed_tool_names: &HashSet<String>,
) -> Vec<serde_json::Value> {
    let mut valid_tool_call_ids = HashSet::new();
    let mut serialized = Vec::with_capacity(messages.len());
    for message in messages {
        match message {
            AgentMessage::AssistantToolCallProtocol { content, calls } => {
                let all_calls_in_scope = calls
                    .iter()
                    .all(|call| allowed_tool_names.contains(&call.name));
                if flatten_unmatched_tool_messages && !all_calls_in_scope {
                    serialized.push(json!({
                        "role": "assistant",
                        "content": summarize_assistant_tool_call_protocol(content.as_deref(), &calls),
                    }));
                    continue;
                }
                if flatten_unmatched_tool_messages {
                    valid_tool_call_ids.extend(calls.iter().map(|call| call.id.clone()));
                }
                serialized.push(agent_message_to_openai_message(
                    AgentMessage::AssistantToolCallProtocol { content, calls },
                ));
            }
            AgentMessage::Tool {
                tool_call_id,
                name,
                content,
            } if flatten_unmatched_tool_messages
                && (!valid_tool_call_ids.contains(&tool_call_id)
                    || !allowed_tool_names.contains(&name)) =>
            {
                serialized.push(json!({
                    "role": "assistant",
                    "content": flatten_tool_result_as_assistant_text(&name, &content),
                }));
            }
            other => serialized.push(agent_message_to_openai_message(other)),
        }
    }
    serialized
}

fn flatten_tool_result_as_assistant_text(name: &str, content: &str) -> String {
    format!("historical tool result ({name}):\n{content}")
}

fn truncate_for_error(text: &str) -> String {
    const MAX_LEN: usize = 600;
    if text.chars().count() <= MAX_LEN {
        return text.to_string();
    }
    let truncated = text.chars().take(MAX_LEN).collect::<String>();
    format!("{truncated}...")
}

fn looks_like_context_window_error(body: &str) -> bool {
    let normalized = body.to_ascii_lowercase();
    normalized.contains("context length")
        || normalized.contains("context window")
        || normalized.contains("maximum context length")
        || normalized.contains("too many tokens")
        || normalized.contains("max context")
}

fn truncate_for_json_error(value: &serde_json::Value) -> String {
    truncate_for_error(&value.to_string())
}

fn parse_retry_after_seconds(value: &str) -> Option<u64> {
    value.trim().parse::<u64>().ok()
}

fn default_rate_limit_backoff(attempt: usize) -> Duration {
    let seconds = match attempt {
        0 => 2,
        1 => 4,
        2 => 8,
        _ => 12,
    };
    Duration::from_secs(seconds)
}

// ---------------------------------------------------------------------------
// CopilotClient：首选 session token（内部 API，全模型），失败降级到公共 API
// ---------------------------------------------------------------------------

const COPILOT_USER_AGENT: &str = "GitHubCopilotChat/0.26.7";
const COPILOT_EDITOR_VERSION: &str = "vscode/1.96.2";
const COPILOT_GITHUB_API_VERSION: &str = "2025-04-01";
const COPILOT_INTERNAL_BASE_URL: &str = "https://api.individual.githubcopilot.com";

struct CopilotSessionToken {
    token: String,
    base_url: String,
    expires_at_secs: u64,
}

pub struct CopilotClient {
    github_token: String,
    auth_client: reqwest::Client,
    cached: tokio::sync::Mutex<Option<CopilotSessionToken>>,
    inner: tokio::sync::Mutex<OpenAIClient>,
}

impl CopilotClient {
    pub fn new(github_token: &str, model_config: &ModelConfig) -> Self {
        let auth_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .expect("failed to build copilot auth http client");
        let inner = OpenAIClient::from_parts("placeholder", COPILOT_INTERNAL_BASE_URL, model_config);
        Self {
            github_token: github_token.to_string(),
            auth_client,
            cached: tokio::sync::Mutex::new(None),
            inner: tokio::sync::Mutex::new(inner),
        }
    }

    async fn ensure_auth(&self) -> Result<()> {
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let needs_exchange = {
            let cached = self.cached.lock().await;
            match cached.as_ref() {
                None => true,
                Some(t) => now_secs + 60 >= t.expires_at_secs,
            }
        };

        if !needs_exchange {
            return Ok(());
        }

        let (token, base_url, expires_at_secs) = self.exchange_session_token().await?;
        tracing::info!(base_url = %base_url, "copilot: session token acquired");

        let mut hdrs = reqwest::header::HeaderMap::new();
        hdrs.insert("Editor-Version", COPILOT_EDITOR_VERSION.parse().unwrap());
        hdrs.insert("User-Agent", COPILOT_USER_AGENT.parse().unwrap());
        hdrs.insert("X-Github-Api-Version", COPILOT_GITHUB_API_VERSION.parse().unwrap());

        let mut inner = self.inner.lock().await;
        inner.api_key = token.clone();
        inner.base_url = base_url.clone();
        inner.completions_path = "/chat/completions";
        inner.extra_headers = hdrs;

        *self.cached.lock().await = Some(CopilotSessionToken { token, base_url, expires_at_secs });
        Ok(())
    }

    async fn exchange_session_token(&self) -> Result<(String, String, u64)> {
        tracing::debug!("copilot: exchanging github token for session token");
        let resp = self.auth_client
            .get("https://api.github.com/copilot_internal/v2/token")
            .header("Authorization", format!("Bearer {}", self.github_token))
            .header("Accept", "application/json")
            .header("User-Agent", COPILOT_USER_AGENT)
            .header("Editor-Version", COPILOT_EDITOR_VERSION)
            .header("X-Github-Api-Version", COPILOT_GITHUB_API_VERSION)
            .send()
            .await
            .map_err(|e| miette!("Copilot token exchange request failed: {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            tracing::debug!(http_status = %status, body = %body, "copilot session token exchange non-2xx");
            return Err(miette!("HTTP {status}"));
        }

        let json: serde_json::Value = resp.json().await
            .map_err(|e| miette!("parse error: {e}"))?;

        let token = json["token"].as_str()
            .ok_or_else(|| miette!("missing 'token' field"))?
            .to_string();
        let expires_at_secs = json["expires_at"].as_u64().unwrap_or(0);
        let base_url = derive_copilot_base_url(&token);

        Ok((token, base_url, expires_at_secs))
    }
}

/// session token 是分号分隔的 key=value 串，从 proxy-ep 字段派生 API base URL。
fn derive_copilot_base_url(session_token: &str) -> String {
    session_token.split(';').find_map(|part| {
        let trimmed = part.trim();
        let val = trimmed.to_lowercase();
        val.strip_prefix("proxy-ep=").and_then(|_| {
            let host = &trimmed[9..];
            if host.is_empty() { return None; }
            let host = if host.to_lowercase().starts_with("proxy.") {
                format!("api.{}", &host[6..])
            } else {
                host.to_string()
            };
            Some(format!("https://{host}"))
        })
    }).unwrap_or_else(|| COPILOT_INTERNAL_BASE_URL.to_string())
}

#[async_trait]
impl LLM for CopilotClient {
    async fn run_json(&self, context: &Context, request: PromptRequest) -> Result<serde_json::Value> {
        self.ensure_auth().await?;
        self.inner.lock().await.run_json(context, request).await
    }

    async fn run_agent_turn(&self, context: &Context, request: AgentTurnRequest) -> Result<AgentTurnStreamResult> {
        self.ensure_auth().await?;
        self.inner.lock().await.run_agent_turn(context, request).await
    }

    fn token_usage_info(&self) -> Option<TokenUsageInfo> {
        self.inner.try_lock().ok()?.token_usage_info()
    }

    fn model_name(&self) -> Option<String> {
        self.inner.try_lock().ok()?.model_name()
    }
}

// ---------------------------------------------------------------------------
// 工厂函数：根据 config 构造 LLM
// ---------------------------------------------------------------------------

/// 根据 model 名称和全局 Config 构造对应的 LLM 实例。
pub fn build_llm(model_name: &str, config: &Config) -> Result<Box<dyn LLM + Send + Sync>> {
    let model_config = config.models.get(model_name).ok_or_else(|| {
        miette!("model '{}' not found in [models]", model_name)
    })?;
    let provider_config = config.providers.get(&model_config.provider).ok_or_else(|| {
        miette!(
            "provider '{}' (referenced by model '{}') not found in [providers]",
            model_config.provider,
            model_name
        )
    })?;

    match provider_config {
        ProviderConfig::Openai { api_key, base_url } => {
            let base = base_url.as_deref().unwrap_or("https://api.openai.com");
            Ok(Box::new(OpenAIClient::from_parts(api_key, base, model_config)))
        }
        ProviderConfig::OpenaiCompatible { base_url, api_key } => {
            Ok(Box::new(OpenAIClient::from_parts(api_key, base_url, model_config)))
        }
        ProviderConfig::GithubCopilot { github_token } => {
            let resolved = resolve_env_var_ref(github_token);
            Ok(Box::new(CopilotClient::new(&resolved, model_config)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reasoning::runtime::{AgentToolCall, HistoryMessage};
    use serde_json::json;

    #[test]
    fn compatible_agent_messages_flatten_unmatched_tool_results() {
        let messages = agent_turn_request_to_openai_messages(
            vec![
                AgentMessage::assistant("assistant tool-call protocol: update_plan"),
                AgentMessage::tool("historical-tool", "historical_tool", "summary=updated plan"),
            ],
            true,
            &HashSet::from(["update_plan".to_string()]),
        );

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1]["role"], "assistant");
        assert!(
            messages[1]["content"]
                .as_str()
                .unwrap_or_default()
                .contains("historical tool result")
        );
    }

    #[test]
    fn compatible_agent_messages_keep_matched_tool_results() {
        let messages = agent_turn_request_to_openai_messages(
            vec![
                AgentMessage::assistant_tool_call_protocol(
                    None,
                    vec![AgentToolCall {
                        id: "call_123".to_string(),
                        name: "update_plan".to_string(),
                        arguments: json!({"plan": []}),
                    }],
                ),
                AgentMessage::tool("call_123", "update_plan", "{\"ok\":true}"),
            ],
            true,
            &HashSet::from(["update_plan".to_string()]),
        );

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1]["role"], "tool");
        assert_eq!(messages[1]["tool_call_id"], "call_123");
    }

    #[test]
    fn compatible_prompt_messages_flatten_historical_tool_role() {
        let request = PromptRequest {
            tool_name: "demo".to_string(),
            tool_description: "demo".to_string(),
            output_schema: json!({"type":"object","properties":{},"required":[]}),
            system_messages: vec![],
            long_term_memory_messages: vec![],
            history_messages: vec![HistoryMessage::tool(
                "call_x",
                "update_plan",
                "tool_call_id=call_x\nname=update_plan\nsummary=updated plan",
                crate::tool_ui::ToolUiEvent::error(
                    "apply_patch".to_string(),
                    vec!["irrelevant".to_string()],
                ),
            )],
            current_user_message: "hello".to_string(),
            retry_messages: vec![],
        };

        let messages = prompt_request_to_openai_messages(request, true);
        assert_eq!(messages[0]["role"], "assistant");
        assert!(
            messages[0]["content"]
                .as_str()
                .unwrap_or_default()
                .contains("historical tool result")
        );
    }

    #[test]
    fn compatible_agent_messages_flatten_out_of_scope_tool_protocol() {
        let messages = agent_turn_request_to_openai_messages(
            vec![AgentMessage::assistant_tool_call_protocol(
                None,
                vec![AgentToolCall {
                    id: "call_123".to_string(),
                    name: "terminal_exec".to_string(),
                    arguments: json!({"cmd": "pwd"}),
                }],
            )],
            true,
            &HashSet::from(["finish_and_send".to_string()]),
        );

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "assistant");
        assert!(messages[0].get("tool_calls").is_none());
    }

    #[test]
    fn thinking_budget_is_injected_as_reasoning_effort_by_default() {
        let mut model_config = ModelConfig::default();
        model_config.thinking_budget = Some("medium".to_string());
        let client = OpenAIClient::from_parts("test-key", "https://api.openai.com", &model_config);

        let payload = build_agent_turn_payload_common(
            &client,
            AgentTurnRequest {
                messages: vec![AgentMessage::user("hello")],
                tools: vec![],
            },
            true,
            false,
        );

        assert_eq!(payload["reasoning_effort"], "medium");
    }

    #[test]
    fn thinking_budget_can_use_nested_reasoning_payload() {
        let mut payload = json!({
            "model": "demo",
            "messages": [],
        });
        apply_optional_thinking_budget(
            &mut payload,
            Some("high"),
            ThinkingBudgetMode::NestedReasoningObject,
        );

        assert_eq!(payload["reasoning"]["effort"], "high");
        assert!(payload.get("reasoning_effort").is_none());
    }

    #[test]
    fn detect_reasoning_effort_and_nested_reasoning_rejections() {
        assert!(should_retry_prompt_request_with_nested_thinking_budget(
            "Unknown parameter: 'reasoning_effort'."
        ));
        assert!(should_retry_request_without_thinking_budget(
            "Unknown parameter: 'reasoning'."
        ));
    }
}
