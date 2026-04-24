//! 本模块实现实际的llm api调用

use std::{
    collections::{HashMap, HashSet, VecDeque},
    error::Error as _,
    sync::{Arc, LazyLock, Mutex},
    time::{Duration, Instant},
};

use async_trait::async_trait;
use futures_util::StreamExt;
use miette::{Result, miette};
use parking_lot::Mutex as ParkingLotMutex;
use serde_json::json;
use tracing::warn;

use crate::{
    config::{Config, ModelConfig, ProviderConfig, normalize_provider_base_url},
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

mod copilot;
pub use copilot::{CopilotClient, exchange_copilot_session_token};

mod io;
use io::*;
const DEEPSEEK_THINKING_MAX_TOKENS: usize = 65_536;

pub struct OpenAIClient {
    client: reqwest::Client,
    pub(crate) api_key: String,
    pub(crate) base_url: String,
    /// chat completions 路径，默认 "/chat/completions"
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
    Omit,
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

impl OpenAIClient {
    /// 从独立的凭据 + ModelConfig 构造。
    pub fn from_parts(api_key: &str, base_url: &str, model_config: &ModelConfig) -> Self {
        let base_url = normalize_provider_base_url(base_url);
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
            base_url: base_url.clone(),
            completions_path: "/chat/completions",
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
            request_rate_limiter: shared_request_rate_limiter(
                &base_url,
                &model_config.model_id,
                model_config.rpm(),
            ),
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
                && adapter_state.prompt_tool_choice_mode == PromptToolChoiceMode::NamedFunction
            {
                adapter_state.prompt_tool_choice_mode = PromptToolChoiceMode::RequiredString;
                self.update_adapter_state(adapter_state);
                warn!(
                    "llm provider rejected named function tool_choice; retrying prompt request with string tool_choice\n{}",
                    request_context.join("\n")
                );
                continue;
            }

            if should_retry_prompt_request_without_tool_choice(&body)
                && adapter_state.prompt_tool_choice_mode != PromptToolChoiceMode::Omit
            {
                adapter_state.prompt_tool_choice_mode = PromptToolChoiceMode::Omit;
                self.update_adapter_state(adapter_state);
                warn!(
                    "llm provider does not support tool_choice; retrying prompt request without tool_choice\n{}",
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
        let mut reasoning_content = String::new();
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
                if let Some(delta_reasoning_content) = delta["reasoning_content"].as_str() {
                    reasoning_content.push_str(delta_reasoning_content);
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
                last_reasoning_content: non_empty_string(reasoning_content),
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
            last_reasoning_content: non_empty_string(reasoning_content),
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
            "max_tokens": max_completion_tokens_for_chat_payload(client),
        });
        apply_provider_thinking_config(
            &mut payload,
            client,
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
            "temperature": client.temperature,
            "max_tokens": max_completion_tokens_for_chat_payload(client),
        });
        match self.state.prompt_tool_choice_mode {
            PromptToolChoiceMode::NamedFunction => {
                payload["tool_choice"] = json!({
                    "type": "function",
                    "function": { "name": request.tool_name }
                });
            }
            PromptToolChoiceMode::RequiredString => {
                payload["tool_choice"] = json!("required");
            }
            PromptToolChoiceMode::Omit => {}
        }
        apply_provider_thinking_config(
            &mut payload,
            client,
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

fn build_agent_turn_payload_common(
    client: &OpenAIClient,
    request: AgentTurnRequest,
    stream: bool,
    flatten_orphan_tool_messages: bool,
) -> serde_json::Value {
    let messages = agent_turn_request_to_openai_messages(
        request.messages,
        flatten_orphan_tool_messages,
        is_deepseek_api_base_url(&client.base_url),
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
        "max_tokens": max_completion_tokens_for_chat_payload(client),
        "stream": stream,
        "stream_options": if stream {
            json!({ "include_usage": true })
        } else {
            serde_json::Value::Null
        },
    });
    apply_provider_thinking_config(
        &mut payload,
        client,
        client.thinking_budget.as_deref(),
        client.adapter_state_guard().thinking_budget_mode,
    );
    payload
}

fn max_completion_tokens_for_chat_payload(client: &OpenAIClient) -> usize {
    if is_deepseek_thinking_request(client) {
        client
            .max_completion_tokens
            .min(DEEPSEEK_THINKING_MAX_TOKENS)
    } else {
        client.max_completion_tokens
    }
}

fn is_deepseek_api_base_url(base_url: &str) -> bool {
    base_url
        .trim_end_matches('/')
        .to_ascii_lowercase()
        .contains("api.deepseek.com")
}

fn is_deepseek_thinking_request(client: &OpenAIClient) -> bool {
    if !is_deepseek_api_base_url(&client.base_url) {
        return false;
    }
    match deepseek_thinking_type(client.thinking_budget.as_deref()) {
        Some("enabled") => return true,
        Some("disabled") => return false,
        _ => {}
    }
    deepseek_model_defaults_to_thinking(&client.model)
}

fn deepseek_model_defaults_to_thinking(model_id: &str) -> bool {
    let model = model_id.to_ascii_lowercase();
    matches!(
        model.as_str(),
        "deepseek-reasoner" | "deepseek-v4-flash" | "deepseek-v4-pro"
    ) || model.ends_with("/deepseek-reasoner")
        || model.ends_with("/deepseek-v4-flash")
        || model.ends_with("/deepseek-v4-pro")
}

fn deepseek_thinking_type(value: Option<&str>) -> Option<&'static str> {
    let value = value?.trim().to_ascii_lowercase();
    if value.is_empty() {
        return None;
    }
    Some(match value.as_str() {
        "0" | "false" | "off" | "none" | "disable" | "disabled" | "no" => "disabled",
        _ => "enabled",
    })
}

fn deepseek_reasoning_effort(value: Option<&str>) -> Option<&'static str> {
    let value = value?.trim().to_ascii_lowercase();
    if value.is_empty() {
        return None;
    }
    match value.as_str() {
        "0" | "false" | "off" | "none" | "disable" | "disabled" | "no" => None,
        "xhigh" | "max" | "maximum" => Some("max"),
        // DeepSeek currently accepts high/max. Treat generic low/medium/high budgets as high.
        _ => Some("high"),
    }
}

fn apply_provider_thinking_config(
    payload: &mut serde_json::Value,
    client: &OpenAIClient,
    thinking_budget: Option<&str>,
    mode: ThinkingBudgetMode,
) {
    if is_deepseek_api_base_url(&client.base_url) {
        apply_optional_deepseek_thinking(payload, thinking_budget);
    } else {
        apply_optional_thinking_budget(payload, thinking_budget, mode);
    }
}

fn apply_optional_deepseek_thinking(
    payload: &mut serde_json::Value,
    thinking_budget: Option<&str>,
) {
    let Some(thinking_type) = deepseek_thinking_type(thinking_budget) else {
        return;
    };
    let Some(object) = payload.as_object_mut() else {
        return;
    };
    object.insert("thinking".to_string(), json!({ "type": thinking_type }));
    if let Some(reasoning_effort) = deepseek_reasoning_effort(thinking_budget)
        && thinking_type == "enabled"
    {
        object.insert("reasoning_effort".to_string(), json!(reasoning_effort));
    }
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

fn agent_message_to_openai_message(
    message: AgentMessage,
    include_reasoning_content: bool,
) -> serde_json::Value {
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
        AgentMessage::AssistantToolCallProtocol {
            content,
            reasoning_content,
            calls,
        } => {
            let mut message = json!({
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
            });
            if include_reasoning_content
                && let Some(reasoning_content) = reasoning_content
                && !reasoning_content.trim().is_empty()
            {
                message["reasoning_content"] = json!(reasoning_content);
            }
            message
        }
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
            AgentMessage::AssistantToolCallProtocol { content, calls, .. } => {
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
    agent_message_to_openai_message(message.clone(), false)
}

fn agent_turn_request_to_openai_messages(
    messages: Vec<AgentMessage>,
    flatten_orphan_tool_messages: bool,
    include_reasoning_content: bool,
) -> Vec<serde_json::Value> {
    let mut valid_tool_call_ids = HashSet::new();
    let mut serialized = Vec::with_capacity(messages.len());
    for message in messages {
        match message {
            AgentMessage::AssistantToolCallProtocol {
                content,
                reasoning_content,
                calls,
            } => {
                if flatten_orphan_tool_messages {
                    valid_tool_call_ids.extend(calls.iter().map(|call| call.id.clone()));
                }
                serialized.push(agent_message_to_openai_message(
                    AgentMessage::AssistantToolCallProtocol {
                        content,
                        reasoning_content,
                        calls,
                    },
                    include_reasoning_content,
                ));
            }
            AgentMessage::Tool {
                tool_call_id,
                name,
                content,
            } if flatten_orphan_tool_messages && !valid_tool_call_ids.contains(&tool_call_id) => {
                serialized.push(json!({
                    "role": "assistant",
                    "content": flatten_tool_result_as_assistant_text(&name, &content),
                }));
            }
            other => serialized.push(agent_message_to_openai_message(other, false)),
        }
    }
    serialized
}

fn flatten_tool_result_as_assistant_text(name: &str, content: &str) -> String {
    format!("historical tool result ({name}):\n{content}")
}

// ---------------------------------------------------------------------------
// 工厂函数：根据 config 构造 LLM
// ---------------------------------------------------------------------------

/// 根据 model 名称和全局 Config 构造对应的 LLM 实例。
pub fn build_llm(model_name: &str, config: &Config) -> Result<Box<dyn LLM + Send + Sync>> {
    let model_config = config
        .models
        .get(model_name)
        .ok_or_else(|| miette!("model '{}' not found in [models]", model_name))?;
    let provider_config = config
        .providers
        .get(&model_config.provider)
        .ok_or_else(|| {
            miette!(
                "provider '{}' (referenced by model '{}') not found in [providers]",
                model_config.provider,
                model_name
            )
        })?;

    match provider_config {
        ProviderConfig::Openai { api_key, base_url } => {
            let base = base_url.as_deref().unwrap_or("https://api.openai.com/v1");
            Ok(Box::new(OpenAIClient::from_parts(
                api_key,
                base,
                model_config,
            )))
        }
        ProviderConfig::OpenaiCompatible { base_url, api_key } => Ok(Box::new(
            OpenAIClient::from_parts(api_key, base_url, model_config),
        )),
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
            false,
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
            false,
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
    fn openai_client_treats_base_url_as_api_root() {
        let model_config = ModelConfig::default();
        let plain = OpenAIClient::from_parts("test-key", "https://api.deepseek.com", &model_config);
        let versioned =
            OpenAIClient::from_parts("test-key", "https://api.deepseek.com/v1/", &model_config);

        assert_eq!(plain.url(), "https://api.deepseek.com/chat/completions");
        assert_eq!(
            versioned.url(),
            "https://api.deepseek.com/v1/chat/completions"
        );
    }

    #[test]
    fn compatible_agent_messages_preserve_out_of_scope_tool_protocol() {
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
            false,
        );

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "assistant");
        assert!(messages[0].get("tool_calls").is_some());
    }

    #[test]
    fn deepseek_reasoning_content_is_forwarded_for_native_tool_protocol() {
        let historical_call = AgentToolCall {
            id: "call_old".to_string(),
            name: "update_plan".to_string(),
            arguments: json!({"plan": []}),
        };
        let current_call = AgentToolCall {
            id: "call_new".to_string(),
            name: "update_plan".to_string(),
            arguments: json!({"plan": []}),
        };
        let messages = agent_turn_request_to_openai_messages(
            vec![
                AgentMessage::assistant_tool_call_protocol_with_reasoning(
                    None,
                    Some("old reasoning".to_string()),
                    vec![historical_call],
                ),
                AgentMessage::user("new task"),
                AgentMessage::assistant_tool_call_protocol_with_reasoning(
                    None,
                    Some("current reasoning".to_string()),
                    vec![current_call],
                ),
            ],
            false,
            true,
        );

        assert_eq!(messages[0]["reasoning_content"], "old reasoning");
        assert_eq!(messages[2]["reasoning_content"], "current reasoning");
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
    fn deepseek_thinking_budget_uses_thinking_and_reasoning_effort_parameters() {
        let mut model_config = ModelConfig::default();
        model_config.model_id = "deepseek-reasoner".to_string();
        model_config.thinking_budget = Some("medium".to_string());
        model_config.max_completion_tokens = 393_216;
        let client =
            OpenAIClient::from_parts("test-key", "https://api.deepseek.com", &model_config);

        let payload = build_agent_turn_payload_common(
            &client,
            AgentTurnRequest {
                messages: vec![AgentMessage::user("hello")],
                tools: vec![],
            },
            true,
            false,
        );

        assert_eq!(payload["thinking"]["type"], "enabled");
        assert_eq!(payload["reasoning_effort"], "high");
        assert_eq!(payload["max_tokens"], DEEPSEEK_THINKING_MAX_TOKENS);
        assert!(payload.get("reasoning").is_none());
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
