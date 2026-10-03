//! Provider clients and LLM API calls.

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
    config::{
        Config, ModelConfig, ProviderConfig, normalize_provider_base_url, resolve_env_reference,
    },
    context_budget::{ContextBudgetExceededError, RequestBudgetLimits},
    core::{ModelProvider, ModelRequestOptions, TokenUsage, TokenUsageInfo},
    dsml_repair,
    reasoning::runtime::{
        AgentContent, AgentContentPart, AgentMessage, AgentToolCall, AgentToolInputSpec,
        AgentToolSpec, AgentTurnItem, AgentTurnRequest, AgentTurnStreamResult, PromptRequest,
        assistant_tool_call_protocol_char_count, summarize_assistant_tool_call_protocol,
    },
};

mod copilot;
pub use copilot::CopilotClient;
pub(crate) use copilot::copilot_base_url_from_session_token;
mod codex_oauth;
pub use codex_oauth::{
    CodexOAuthClient, CodexOAuthTokens, codex_cli_auth_file, codex_oauth_access_from_file,
    codex_oauth_client_version, codex_oauth_default_base_url, import_codex_cli_oauth_file,
    imported_codex_oauth_auth_file, write_codex_oauth_tokens,
};
mod opencode_oauth;
pub use opencode_oauth::{
    OPENCODE_OAUTH_DEFAULT_SERVER, OPENCODE_ZEN_BASE_URL, OpenCodeDevicePoll, OpenCodeOAuthClient,
    opencode_auth_file_path, opencode_console_access, opencode_poll_device_token,
    opencode_start_device_code, write_opencode_oauth_tokens,
};
mod ollama;
pub use ollama::OllamaClient;

mod anthropic_compat;
pub mod responses_compat;

mod io;
use io::{
    StreamingToolCallBuilder, default_rate_limit_backoff, format_request_error,
    looks_like_context_window_error, looks_like_vision_unsupported_error, non_empty_string,
    normalize_sse_buffer, parse_agent_turn_stream_result_from_json, parse_retry_after_seconds,
    parse_usage_from_response_json, read_response_text_with_timeout,
    send_request_for_streaming_response, should_retry_prompt_request_with_nested_thinking_budget,
    should_retry_request_without_reasoning_content, should_retry_request_without_thinking_budget,
    summarize_agent_turn_request, summarize_prompt_request, take_next_sse_event,
    truncate_for_error, truncate_for_json_error,
};
mod payload;
mod thinking;
#[cfg(test)]
use payload::{agent_message_to_openai_message, agent_turn_request_to_openai_messages};
use payload::{
    build_agent_turn_payload_common, flatten_tool_result_as_assistant_text,
    prompt_request_to_openai_messages,
};
#[cfg(test)]
use thinking::{DEEPSEEK_THINKING_MAX_TOKENS, apply_optional_thinking_budget};
use thinking::{apply_provider_thinking_config, max_completion_tokens_for_chat_payload};
pub struct OpenAIClient {
    client: reqwest::Client,
    pub(crate) api_key: String,
    pub(crate) base_url: String,
    /// Chat completions path. Defaults to "/chat/completions".
    completions_path: &'static str,
    /// Extra headers attached to each request, for example Copilot IDE auth.
    extra_headers: reqwest::header::HeaderMap,
    model: String,
    temperature: f64,
    thinking_budget: Option<String>,
    rpm: Option<usize>,
    request_timeout: Duration,
    stream_idle_timeout: Duration,
    context_window_tokens: usize,
    effective_context_window_tokens: usize,
    auto_compact_threshold_tokens: usize,
    reserved_output_tokens: usize,
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
enum ToolStrictMode {
    Enabled,
    Omitted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ThinkingBudgetMode {
    ReasoningEffortString,
    NestedReasoningObject,
    Unsupported,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VisionMode {
    /// Model accepts `image_url` content (default assumption).
    Enabled,
    /// Model rejected `image_url`; strip all images before sending.
    Disabled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReasoningContentMode {
    /// Try forwarding non-empty provider `reasoning_content` in historical assistant messages.
    Enabled,
    /// Provider rejected `reasoning_content` message fields; strip them when serializing.
    Disabled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ChatCompletionsAdapterState {
    prompt_tool_choice_mode: PromptToolChoiceMode,
    tool_strict_mode: ToolStrictMode,
    thinking_budget_mode: ThinkingBudgetMode,
    vision_mode: VisionMode,
    reasoning_content_mode: ReasoningContentMode,
}

impl Default for ChatCompletionsAdapterState {
    fn default() -> Self {
        Self {
            prompt_tool_choice_mode: PromptToolChoiceMode::NamedFunction,
            tool_strict_mode: ToolStrictMode::Enabled,
            thinking_budget_mode: ThinkingBudgetMode::ReasoningEffortString,
            vision_mode: VisionMode::Enabled,
            reasoning_content_mode: ReasoningContentMode::Enabled,
        }
    }
}

type RequestRateLimiter = Arc<tokio::sync::Mutex<VecDeque<Instant>>>;
type RequestRateLimiterMap = HashMap<String, RequestRateLimiter>;

static REQUEST_RATE_LIMITERS: LazyLock<ParkingLotMutex<RequestRateLimiterMap>> =
    LazyLock::new(|| ParkingLotMutex::new(HashMap::new()));

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

    /// Build from standalone credentials and `ModelConfig`.
    pub fn from_parts(api_key: &str, base_url: &str, model_config: &ModelConfig) -> Self {
        let base_url = normalize_provider_base_url(base_url);
        let request_timeout = Duration::from_secs(model_config.request_timeout_secs());
        let stream_idle_timeout = Duration::from_secs(model_config.stream_idle_timeout_secs());
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .build()
            .expect("failed to build llm http client");
        let context_window_tokens = model_config.context_window_tokens();
        let effective_context_window_tokens = model_config.effective_context_window_tokens();
        let auto_compact_threshold_tokens = model_config.auto_compact_token_limit();
        // Chat Completions requires a positive output limit. Keep the runtime
        // budget and the serialized max_tokens field on the same minimum.
        let reserved_output_tokens = model_config.reserved_output_tokens().max(1);
        let max_completion_tokens = model_config.max_completion_tokens();
        Self {
            client,
            api_key: api_key.to_string(),
            base_url: base_url.clone(),
            completions_path: "/chat/completions",
            extra_headers: reqwest::header::HeaderMap::new(),
            model: model_config.model_id.clone(),
            temperature: model_config.temperature,
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
            max_completion_tokens,
            request_rate_limiter: shared_request_rate_limiter(
                &base_url,
                &model_config.model_id,
                model_config.rpm(),
            ),
            adapter_state: Mutex::new({
                use crate::model_catalog::catalog_model_capacity;
                let vision_mode = match model_config.supports_vision {
                    Some(true) => VisionMode::Enabled,
                    Some(false) => VisionMode::Disabled,
                    None => {
                        let supports = catalog_model_capacity(&model_config.model_id)
                            .is_some_and(|c| c.supports_vision);
                        if supports {
                            VisionMode::Enabled
                        } else {
                            VisionMode::Disabled
                        }
                    }
                };
                ChatCompletionsAdapterState {
                    vision_mode,
                    ..ChatCompletionsAdapterState::default()
                }
            }),
            token_usage: Mutex::new(TokenUsageInfo {
                total_token_usage: TokenUsage::default(),
                last_token_usage: TokenUsage::default(),
                model_context_window: i64::try_from(context_window_tokens).ok(),
                daily_token_usage: Vec::new(),
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

    const fn request_budget_limits(&self) -> RequestBudgetLimits {
        RequestBudgetLimits {
            context_window_tokens: self.effective_context_window_tokens,
            auto_compact_threshold_tokens: self.auto_compact_threshold_tokens,
            reserved_output_tokens: self.reserved_output_tokens,
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
                    if now.duration_since(front) >= Duration::from_mins(1) {
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
                        Duration::from_mins(1).saturating_sub(now.duration_since(front))
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
        response_header_timeout: Duration,
        session_headers: &reqwest::header::HeaderMap,
    ) -> Result<reqwest::Response> {
        const MAX_429_RETRIES: usize = 4;
        const MAX_5XX_RETRIES: usize = 3;

        let mut request_headers = self.extra_headers.clone();
        request_headers.extend(
            session_headers
                .iter()
                .map(|(name, value)| (name.clone(), value.clone())),
        );
        let mut rate_limit_attempt = 0usize;
        let mut transient_attempt = 0usize;
        loop {
            self.wait_for_request_slot(request_context).await;
            let request = self
                .client
                .post(url)
                .bearer_auth(&self.api_key)
                .headers(request_headers.clone())
                .json(payload);
            let response = send_request_for_streaming_response(
                request,
                response_header_timeout,
                "llm request failed",
                url,
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
                    response_header_timeout,
                    "llm 429 response body read failed",
                    url,
                    request_context,
                )
                .await?;

                if rate_limit_attempt >= MAX_429_RETRIES {
                    return Err(miette!(
                        "llm api returned HTTP 429 after {} retries: {}",
                        MAX_429_RETRIES,
                        truncate_for_error(&body)
                    ));
                }

                let delay = retry_after.map_or_else(
                    || default_rate_limit_backoff(rate_limit_attempt),
                    Duration::from_secs,
                );
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
                let body = read_response_text_with_timeout(
                    response,
                    response_header_timeout,
                    "llm 5xx response body read failed",
                    url,
                    request_context,
                )
                .await?;

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

    /// Step adapter state to the next downgrade level on a 400 response.
    /// Order: omit function `strict`, downgrade `tool_choice`
    /// (`NamedFunction` → `RequiredString` → Omit), then downgrade the
    /// thinking budget (`ReasoningEffortString` → `NestedReasoningObject` → Unsupported).
    /// Returns false when no further downgrade is available.
    fn step_adapter_state_for_bad_request(
        state: &mut ChatCompletionsAdapterState,
        has_thinking_budget: bool,
        allow_tool_strict_downgrade: bool,
    ) -> bool {
        if allow_tool_strict_downgrade && Self::disable_tool_strict_for_bad_request(state, true) {
            return true;
        }
        if state.prompt_tool_choice_mode == PromptToolChoiceMode::NamedFunction {
            state.prompt_tool_choice_mode = PromptToolChoiceMode::RequiredString;
            return true;
        }
        if state.prompt_tool_choice_mode != PromptToolChoiceMode::Omit {
            state.prompt_tool_choice_mode = PromptToolChoiceMode::Omit;
            return true;
        }
        if has_thinking_budget {
            if state.thinking_budget_mode == ThinkingBudgetMode::ReasoningEffortString {
                state.thinking_budget_mode = ThinkingBudgetMode::NestedReasoningObject;
                return true;
            }
            if state.thinking_budget_mode != ThinkingBudgetMode::Unsupported {
                state.thinking_budget_mode = ThinkingBudgetMode::Unsupported;
                return true;
            }
        }
        false
    }

    fn disable_tool_strict_for_bad_request(
        state: &mut ChatCompletionsAdapterState,
        has_tools: bool,
    ) -> bool {
        if has_tools && state.tool_strict_mode == ToolStrictMode::Enabled {
            state.tool_strict_mode = ToolStrictMode::Omitted;
            return true;
        }
        false
    }
    async fn call_tool_json(
        &self,
        request: PromptRequest,
        options: &ModelRequestOptions,
    ) -> Result<serde_json::Value> {
        let url = self.url();
        let output_schema = request.output_schema.clone();
        let budget = &options.budget;
        let request_context = summarize_prompt_request(&request, Some(budget));
        let session_headers =
            opencode_gateway_headers(&self.base_url, options.conversation_id.as_deref());
        let mut adapter_state = self.adapter_state_guard();
        let body = loop {
            let payload =
                self.current_adapter()
                    .build_prompt_payload(self, &request, output_schema.clone());
            let response = self
                .post_json_with_rate_limit_retry(
                    &url,
                    &payload,
                    &request_context,
                    self.stream_idle_timeout,
                    &session_headers,
                )
                .await?;
            let status = response.status();
            let body = read_response_text_with_timeout(
                response,
                self.stream_idle_timeout,
                "llm response body read failed",
                &url,
                &request_context,
            )
            .await?;

            if status.is_success() {
                break body;
            }

            if looks_like_context_window_error(&body) {
                return Err(ContextBudgetExceededError::for_request(
                    "prompt request",
                    &self.model,
                    budget,
                    Some(&format!(
                        "provider_status={status}; provider_body={}",
                        truncate_for_error(&body)
                    )),
                )
                .into());
            }

            // On 400, step through adapter compatibility modes in order:
            // Omit function strict first, then downgrade tool_choice and the thinking budget.
            // Providers may reject parameters with generic error messages
            // that don't name the specific parameter, so we always try the
            // next mode instead of matching error text.
            if Self::step_adapter_state_for_bad_request(
                &mut adapter_state,
                self.thinking_budget.is_some(),
                !is_standard_openai_base_url(&self.base_url),
            ) {
                self.update_adapter_state(adapter_state);
                warn!(
                    "llm api returned 400; retrying prompt request with downgraded adapter (tool_strict={:?}, tool_choice={:?}, thinking={:?})\n{}",
                    adapter_state.tool_strict_mode,
                    adapter_state.prompt_tool_choice_mode,
                    adapter_state.thinking_budget_mode,
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
        let Some(first_tool_call) = tool_calls.first() else {
            if let Some(value) = extract_json_value_from_content(content) {
                return Ok(value);
            }
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
        request: AgentTurnRequest,
        options: &ModelRequestOptions,
    ) -> Result<AgentTurnStreamResult> {
        let url = self.url();
        let budget = &options.budget;
        let request_context = summarize_agent_turn_request(&request, Some(budget));
        let request_has_tools = !request.tools.is_empty();
        let session_headers =
            opencode_gateway_headers(&self.base_url, options.conversation_id.as_deref());
        let request_has_reasoning_content = request.messages.iter().any(|message| {
            matches!(
                message,
                AgentMessage::AssistantToolCallProtocol {
                    reasoning_content: Some(reasoning_content),
                    ..
                } if !reasoning_content.trim().is_empty()
            )
        });
        let mut adapter_state = self.adapter_state_guard();
        let (response, content_type) = loop {
            let payload =
                self.current_adapter()
                    .build_agent_turn_payload(self, request.clone(), true);
            let response = self
                .post_json_with_rate_limit_retry(
                    &url,
                    &payload,
                    &request_context,
                    self.request_timeout,
                    &session_headers,
                )
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

            let body = read_response_text_with_timeout(
                response,
                self.request_timeout,
                "llm response body read failed",
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

            if request_has_reasoning_content
                && adapter_state.reasoning_content_mode == ReasoningContentMode::Enabled
                && should_retry_request_without_reasoning_content(&body)
            {
                adapter_state.reasoning_content_mode = ReasoningContentMode::Disabled;
                self.update_adapter_state(adapter_state);
                warn!(
                    "llm provider rejected reasoning_content message fields; retrying agent turn without them\n{}",
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

            if looks_like_vision_unsupported_error(&body)
                && adapter_state.vision_mode == VisionMode::Enabled
            {
                adapter_state.vision_mode = VisionMode::Disabled;
                self.update_adapter_state(adapter_state);
                warn!(
                    "llm provider rejected image input; retrying agent turn without images\n{}",
                    request_context.join("\n")
                );
                continue;
            }

            if status == reqwest::StatusCode::BAD_REQUEST
                && !is_standard_openai_base_url(&self.base_url)
                && Self::disable_tool_strict_for_bad_request(&mut adapter_state, request_has_tools)
            {
                self.update_adapter_state(adapter_state);
                warn!(
                    "llm provider returned HTTP 400; retrying agent turn without function strict\n{}",
                    request_context.join("\n")
                );
                continue;
            }

            if status == reqwest::StatusCode::BAD_REQUEST
                && request_has_reasoning_content
                && adapter_state.reasoning_content_mode == ReasoningContentMode::Enabled
            {
                adapter_state.reasoning_content_mode = ReasoningContentMode::Disabled;
                self.update_adapter_state(adapter_state);
                warn!(
                    "llm provider returned HTTP 400 with no recognized error pattern; retrying agent turn without reasoning_content as fallback\n{}",
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

        if !content_type_is_event_stream(&content_type) {
            let body = read_response_text_with_timeout(
                response,
                self.request_timeout,
                "llm response body read failed",
                &url,
                &request_context,
            )
            .await?;
            let response_json: serde_json::Value = serde_json::from_str(&body).map_err(|err| {
                miette!(
                    "llm response is not valid JSON: {err}; body={}",
                    truncate_for_error(&body)
                )
            })?;
            self.record_usage_from_response(&response_json);
            let mut result = parse_agent_turn_stream_result_from_json(&response_json)?;
            result = repair_dsml_in_stream_result(result, &request.tools);
            return Ok(result);
        }

        let allowed_tool_names: HashSet<String> =
            request.tools.iter().map(|t| t.name.clone()).collect();
        let allowed_tool_names = if allowed_tool_names.is_empty() {
            None
        } else {
            Some(allowed_tool_names)
        };

        let mut buffer = Vec::new();
        let mut content = String::new();
        let mut reasoning_content = String::new();
        let mut tool_calls: Vec<StreamingToolCallBuilder> = Vec::new();
        let mut last_usage = None;
        let mut last_assistant_progress_emit_at = Instant::now();
        let mut last_assistant_progress_char_len = 0usize;
        let mut last_reasoning_progress_emit_at = Instant::now();
        let mut last_reasoning_progress_char_len = 0usize;
        let mut stream = response.bytes_stream();
        let stream_request_context = [
            format!("model={}", self.model),
            "phase=chat_completions_stream".to_string(),
        ];
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
            let chunk = chunk.map_err(|err| {
                format_request_error(
                    "llm streaming chunk read failed",
                    &url,
                    &stream_request_context,
                    &err,
                )
            })?;
            buffer.extend_from_slice(&chunk);
            normalize_sse_buffer(&mut buffer);
            while let Some(event) = take_next_sse_event(&mut buffer) {
                let Some(data) = sse_event_data(&event) else {
                    continue;
                };
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
                        .saturating_sub(last_assistant_progress_char_len)
                        >= 64
                        || last_assistant_progress_emit_at.elapsed() >= Duration::from_millis(800);
                    if should_emit && !content.trim().is_empty() {
                        if let Some(progress) = options.progress.as_ref() {
                            progress.emit_assistant_content(content.clone());
                        }
                        last_assistant_progress_emit_at = Instant::now();
                        last_assistant_progress_char_len = content.chars().count();
                    }
                }
                if let Some(delta_reasoning_content) = delta["reasoning_content"].as_str() {
                    reasoning_content.push_str(delta_reasoning_content);
                    let should_emit = reasoning_content
                        .chars()
                        .count()
                        .saturating_sub(last_reasoning_progress_char_len)
                        >= 64
                        || last_reasoning_progress_emit_at.elapsed() >= Duration::from_millis(800);
                    if should_emit && !reasoning_content.trim().is_empty() {
                        if let Some(progress) = options.progress.as_ref() {
                            progress.emit_reasoning_content(reasoning_content.clone());
                        }
                        last_reasoning_progress_emit_at = Instant::now();
                        last_reasoning_progress_char_len = reasoning_content.chars().count();
                    }
                }
                if let Some(delta_tool_calls) = delta["tool_calls"].as_array() {
                    for tool_call in delta_tool_calls {
                        let Some(index) = tool_call["index"]
                            .as_u64()
                            .and_then(|index| usize::try_from(index).ok())
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
        if !reasoning_content.trim().is_empty()
            && reasoning_content.chars().count() != last_reasoning_progress_char_len
            && let Some(progress) = options.progress.as_ref()
        {
            progress.emit_reasoning_content(reasoning_content.clone());
        }
        if !content.trim().is_empty()
            && content.chars().count() != last_assistant_progress_char_len
            && let Some(progress) = options.progress.as_ref()
        {
            progress.emit_assistant_content(content.clone());
        }
        if let Some(usage) = last_usage {
            self.record_last_usage(usage);
        }

        // Some OpenAI-compatible proxies emit tool_call deltas with a non-zero
        // `index`, which leaves lower slots as empty placeholders. Drop those
        // placeholders so they are not mistaken for incomplete tool calls.
        let tool_calls: Vec<StreamingToolCallBuilder> = tool_calls
            .into_iter()
            .filter(|builder| !builder.is_empty())
            .collect();
        if !tool_calls.is_empty() {
            let mut calls = Vec::with_capacity(tool_calls.len());
            for (index, builder) in tool_calls.into_iter().enumerate() {
                let mut call = builder.try_build().ok_or_else(|| {
                    miette!(
                        "llm streaming response ended with incomplete tool call at index {index}"
                    )
                })?;
                dsml_repair::repair_tool_call_arguments(&mut call);
                calls.push(call);
            }
            let cleaned_content = dsml_repair::strip_dsml_from_thinking(&content);
            let cleaned_reasoning = dsml_repair::strip_dsml_from_thinking(&reasoning_content);
            let assistant_message = if cleaned_content.trim().is_empty() {
                None
            } else {
                Some(cleaned_content)
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
                last_reasoning_content: non_empty_string(cleaned_reasoning),
                last_reasoning_signature: None,
            });
        }

        let scavenged_calls = allowed_tool_names
            .as_ref()
            .map_or_else(Vec::new, |allowed| {
                let combined = format!("{reasoning_content}\n{content}");
                dsml_repair::scavenge_dsml_tool_calls(&combined, allowed, 4)
            });

        let cleaned_content = dsml_repair::strip_dsml_from_thinking(&content);
        let cleaned_reasoning = dsml_repair::strip_dsml_from_thinking(&reasoning_content);

        if !scavenged_calls.is_empty() {
            let assistant_message = if cleaned_content.trim().is_empty() {
                None
            } else {
                Some(cleaned_content)
            };
            let mut items = Vec::with_capacity(
                scavenged_calls.len() + usize::from(assistant_message.is_some()),
            );
            if let Some(content) = assistant_message.clone() {
                items.push(AgentTurnItem::AssistantMessage { content });
            }
            items.extend(
                scavenged_calls
                    .into_iter()
                    .map(|call| AgentTurnItem::ToolCall { call }),
            );
            return Ok(AgentTurnStreamResult {
                items,
                raw_stream_follow_up: true,
                last_assistant_message: assistant_message,
                last_reasoning_content: non_empty_string(cleaned_reasoning),
                last_reasoning_signature: None,
            });
        }

        let last_assistant_message = if cleaned_content.trim().is_empty() {
            None
        } else {
            Some(cleaned_content)
        };
        Ok(AgentTurnStreamResult {
            items: last_assistant_message
                .clone()
                .into_iter()
                .map(|content| AgentTurnItem::AssistantMessage { content })
                .collect(),
            raw_stream_follow_up: false,
            last_assistant_message,
            last_reasoning_content: non_empty_string(cleaned_reasoning),
            last_reasoning_signature: None,
        })
    }

    fn record_usage_from_response(&self, response_json: &serde_json::Value) {
        if let Some(usage) = parse_usage_from_response_json(response_json) {
            self.record_last_usage(usage);
        }
    }

    fn record_last_usage(&self, usage: TokenUsage) {
        if let Ok(mut info) = self.token_usage.lock() {
            info.model_context_window = i64::try_from(self.context_window_tokens).ok();
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
        build_agent_turn_payload_common(
            client,
            request,
            stream,
            false,
            false,
            ToolStrictMode::Enabled,
        )
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
        if self.state.tool_strict_mode == ToolStrictMode::Omitted
            && let Some(function) = payload["tools"][0]["function"].as_object_mut()
        {
            function.remove("strict");
        }
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
        build_agent_turn_payload_common(
            client,
            request,
            stream,
            true,
            self.state.reasoning_content_mode == ReasoningContentMode::Enabled,
            self.state.tool_strict_mode,
        )
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
    http_url_host(base_url).is_some_and(|host| host.eq_ignore_ascii_case("api.openai.com"))
}

const OPENCODE_CLIENT_ID: &str = "cli";
const OPENCODE_PROJECT_ID: &str = "global";
/// OpenCode's gateway fingerprints requests against its own CLI, so third-party
/// clients have to present the CLI's full user agent (`opencode/<version>`
/// plus the ai-sdk/bun runtime suffixes) to reach the free tier.
const OPENCODE_USER_AGENT: &str =
    "opencode/1.18.34 ai-sdk/provider-utils/4.0.40 runtime/bun/1.3.14";

static OPENCODE_ID_TIMESTAMP: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
static OPENCODE_ID_COUNTER: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
static OPENCODE_SESSION_IDS: LazyLock<Mutex<HashMap<String, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Reproduce the OpenCode CLI identifier algorithm (`packages/schema/identifier.ts`):
/// `hex48(timestamp_ms * 0x1000 + counter)` (bit-inverted for descending ids)
/// followed by 14 base62 random characters. The gateway validates the shape of
/// `x-opencode-session`/`x-opencode-request`, so arbitrary values are rejected.
fn opencode_identifier(descending: bool) -> String {
    use std::fmt::Write as _;
    use std::sync::atomic::Ordering;

    const CHARS: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    let timestamp = chrono::Utc::now().timestamp_millis();
    let previous = OPENCODE_ID_TIMESTAMP.swap(timestamp, Ordering::Relaxed);
    let counter = if timestamp != previous {
        OPENCODE_ID_COUNTER.store(1, Ordering::Relaxed);
        1
    } else {
        OPENCODE_ID_COUNTER
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1)
    };
    let mut value = (timestamp as u64)
        .wrapping_mul(0x1000)
        .wrapping_add(counter as u64);
    if descending {
        value = !value;
    }
    let mut id = String::with_capacity(26);
    for index in 0..6 {
        let byte = ((value >> (40 - 8 * index)) & 0xff) as u8;
        let _ = write!(id, "{byte:02x}");
    }
    for byte in uuid::Uuid::new_v4().into_bytes().iter().take(14) {
        id.push(CHARS[usize::from(*byte) % CHARS.len()] as char);
    }
    id
}

/// One stable `ses_` id per Daat conversation, mirroring the CLI's per-session id.
fn opencode_session_id(conversation_id: &str) -> String {
    let mut sessions = OPENCODE_SESSION_IDS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    sessions
        .entry(conversation_id.to_string())
        .or_insert_with(|| format!("ses_{}", opencode_identifier(true)))
        .clone()
}

/// The OpenCode Zen/Go gateway (`https://opencode.ai/...`) rejects requests
/// without a stable per-conversation `x-opencode-session` header.
pub(crate) fn is_opencode_gateway_base_url(base_url: &str) -> bool {
    match opencode_gateway_host(base_url) {
        Some(host) => {
            let host = host.to_ascii_lowercase();
            host == "opencode.ai" || host.ends_with(".opencode.ai")
        }
        None => false,
    }
}

fn opencode_gateway_host(base_url: &str) -> Option<String> {
    http_url_host(base_url)
}

/// Host of an `http`/`https` URL, including bracketed IPv6 literals.
///
/// Returns `None` when the input is not an absolute HTTP(S) URL or has no host.
/// Path, query, userinfo, and port never contribute to the host.
fn http_url_host(raw: &str) -> Option<String> {
    let url = url::Url::parse(raw.trim()).ok()?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return None;
    }
    match url.host()? {
        url::Host::Domain(domain) => Some(domain.to_string()),
        url::Host::Ipv4(addr) => Some(addr.to_string()),
        url::Host::Ipv6(addr) => Some(format!("[{addr}]")),
    }
}

/// True when the Content-Type media type is `text/event-stream`.
///
/// Parameters (for example `charset`) are ignored and matching is case-insensitive.
/// A substring inside another type or a parameter does not match.
fn content_type_is_event_stream(content_type: &str) -> bool {
    let media_type = content_type.split(';').next().unwrap_or("").trim();
    let Some((type_, subtype)) = media_type.split_once('/') else {
        return false;
    };
    !type_.is_empty()
        && !subtype.is_empty()
        && !subtype.contains('/')
        && type_.eq_ignore_ascii_case("text")
        && subtype.eq_ignore_ascii_case("event-stream")
}

/// Join `data:` fields from one SSE event.
///
/// Follows the SSE spec: a single optional space after `data:` is removed, comment
/// lines are ignored, and multiple data lines are joined with `\n`. The `event:`
/// field is recognized so it is not treated as data. Returns `None` when the event
/// has no data field.
fn sse_event_data(event: &str) -> Option<String> {
    let mut data_lines = Vec::new();
    let mut saw_data = false;
    for line in event.lines() {
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "data" => {
                saw_data = true;
                data_lines.push(value);
            }
            "event" | "id" | "retry" => {}
            _ => {}
        }
    }
    saw_data.then(|| data_lines.join("\n"))
}

/// Remove one leading and one trailing markdown fence, if both are present.
///
/// Only a single fenced block is accepted. Surrounding prose is left untouched so
/// later JSON parsing fails instead of extracting an embedded snippet.
fn strip_single_markdown_fence(content: &str) -> &str {
    let Some(rest) = content.strip_prefix("```") else {
        return content;
    };
    let rest = rest
        .strip_prefix("json")
        .or_else(|| rest.strip_prefix("JSON"))
        .unwrap_or(rest);
    let rest = rest.strip_prefix(['\r', '\n']).unwrap_or(rest);
    let Some(stripped) = rest.strip_suffix("```") else {
        return content;
    };
    let stripped = stripped.trim_end_matches(['\r', '\n', ' ', '\t']);
    if stripped.contains("```") {
        return content;
    }
    stripped
}

pub(crate) fn opencode_gateway_headers(
    base_url: &str,
    conversation_id: Option<&str>,
) -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    if !is_opencode_gateway_base_url(base_url) {
        return headers;
    }
    let Some(conversation_id) = conversation_id else {
        return headers;
    };
    if let Ok(value) = reqwest::header::HeaderValue::from_str(&opencode_session_id(conversation_id))
    {
        headers.insert("x-opencode-session", value);
    }
    if let Ok(value) =
        reqwest::header::HeaderValue::from_str(&format!("msg_{}", opencode_identifier(false)))
    {
        headers.insert("x-opencode-request", value);
    }
    headers.insert("x-opencode-client", OPENCODE_CLIENT_ID.parse().unwrap());
    headers.insert("x-opencode-project", OPENCODE_PROJECT_ID.parse().unwrap());
    headers.insert("x-opencode-project", OPENCODE_PROJECT_ID.parse().unwrap());
    headers.insert(
        reqwest::header::USER_AGENT,
        OPENCODE_USER_AGENT.parse().unwrap(),
    );
    headers
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
    let candidate = strip_single_markdown_fence(trimmed).trim();
    if candidate.is_empty() {
        return None;
    }
    serde_json::from_str::<serde_json::Value>(candidate).ok()
}

#[async_trait]
impl ModelProvider for OpenAIClient {
    async fn complete_json(
        &self,
        request: PromptRequest,
        options: ModelRequestOptions,
    ) -> Result<serde_json::Value> {
        self.call_tool_json(request, &options).await
    }

    async fn complete_agent_turn(
        &self,
        request: AgentTurnRequest,
        options: ModelRequestOptions,
    ) -> Result<AgentTurnStreamResult> {
        self.call_agent_turn(request, &options).await
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
// LLM factory from config.
// ---------------------------------------------------------------------------

/// Build the matching model provider instance by model name and global Config.
pub fn build_model_provider(
    model_name: &str,
    config: &Config,
) -> Result<Box<dyn ModelProvider + Send + Sync>> {
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

    let provider: Box<dyn ModelProvider + Send + Sync> = match provider_config {
        ProviderConfig::Openai { api_key, base_url } => {
            let base = base_url.as_deref().unwrap_or("https://api.openai.com/v1");
            let api_key = resolve_env_reference(api_key);
            Box::new(OpenAIClient::from_parts(&api_key, base, model_config))
        }
        ProviderConfig::AnthropicCompatible { base_url, api_key } => {
            let api_key = resolve_env_reference(api_key);
            Box::new(anthropic_compat::AnthropicCompatibleClient::new(
                &api_key,
                base_url,
                model_config,
            ))
        }
        ProviderConfig::OpenaiCompatible { base_url, api_key } => {
            let api_key = resolve_env_reference(api_key);
            if model_config.api_style.as_deref() == Some("responses") {
                Box::new(responses_compat::ResponsesCompatibleClient::new(
                    &api_key,
                    base_url,
                    model_config,
                ))
            } else {
                Box::new(OpenAIClient::from_parts(&api_key, base_url, model_config))
            }
        }
        ProviderConfig::GithubCopilot { github_token } => {
            let resolved = resolve_env_reference(github_token);
            Box::new(CopilotClient::new(&resolved, model_config))
        }
        ProviderConfig::OpenaiCodexOauth {
            base_url,
            auth_file,
        } => Box::new(CodexOAuthClient::new(
            auth_file.into(),
            base_url.as_deref(),
            model_config,
        )),
        ProviderConfig::OpenCodeConsoleOauth {
            base_url,
            auth_file,
        } => Box::new(OpenCodeOAuthClient::new(
            auth_file.into(),
            base_url.as_deref(),
            model_config,
        )),
        ProviderConfig::Ollama {
            host,
            api_key,
            keep_alive,
        } => Box::new(OllamaClient::from_parts(
            host.as_deref(),
            model_config,
            api_key.as_deref(),
            keep_alive.as_deref(),
        )),
    };
    let mut provider: Box<dyn ModelProvider + Send + Sync> = provider;
    // The gateway only serves its free tier to requests shaped like its own CLI.
    if matches!(
        provider_config,
        ProviderConfig::OpenaiCompatible { base_url, .. }
            if is_opencode_gateway_base_url(base_url)
    ) || matches!(
        provider_config,
        ProviderConfig::OpenCodeConsoleOauth { base_url, .. }
            if base_url.as_deref().is_none_or(is_opencode_gateway_base_url)
    ) {
        provider = Box::new(OpenCodeGatewayProvider { inner: provider });
    }
    Ok(
        match model_facing_tool_name_prefix(&model_config.model_id) {
            Some(prefix) => Box::new(PrefixedToolNameProvider {
                inner: provider,
                prefix,
            }),
            None => provider,
        },
    )
}

/// Prefix applied to the tool names Daat shows a model.
///
/// Some models were trained against another agent's built-in tool surface —
/// Claude Code's `Read` tool takes `file_path`/`offset`/`limit`, for example —
/// and carry a memorized signature for those names. When Daat declares a tool
/// called `read_file`, such a model ignores the declared schema and emits the
/// memorized fields instead, which our strict argument parsing then rejects and
/// the turn loops on failures. Namespacing the tool names removes the collision.
const MODEL_FACING_TOOL_NAME_PREFIX: &str = "dl_";

/// Whether model-facing tool names should be namespaced for this model id.
fn model_facing_tool_name_prefix(model_id: &str) -> Option<&'static str> {
    model_id
        .trim()
        .to_ascii_lowercase()
        .starts_with("claude")
        .then_some(MODEL_FACING_TOOL_NAME_PREFIX)
}

/// Provider decorator that namespaces tool names for models whose own prior
/// collides with ours. The prefix never leaves the provider boundary, so the
/// rest of the runtime keeps using the canonical tool names.
struct PrefixedToolNameProvider {
    inner: Box<dyn ModelProvider + Send + Sync>,
    prefix: &'static str,
}

impl PrefixedToolNameProvider {
    fn model_facing(&self, name: &str) -> String {
        format!("{}{name}", self.prefix)
    }

    fn canonical(&self, name: &str) -> String {
        name.strip_prefix(self.prefix).unwrap_or(name).to_owned()
    }
}

#[async_trait]
impl ModelProvider for PrefixedToolNameProvider {
    async fn complete_json(
        &self,
        request: PromptRequest,
        options: ModelRequestOptions,
    ) -> Result<serde_json::Value> {
        // The structured-output tool name is Daat-internal and does not collide
        // with an agent tool surface, so it stays untouched.
        self.inner.complete_json(request, options).await
    }

    async fn complete_agent_turn(
        &self,
        mut request: AgentTurnRequest,
        options: ModelRequestOptions,
    ) -> Result<AgentTurnStreamResult> {
        for tool in &mut request.tools {
            let name = self.model_facing(&tool.name);
            tool.name = name;
        }
        for message in &mut request.messages {
            match message {
                AgentMessage::AssistantToolCallProtocol { calls, .. } => {
                    for call in calls {
                        let name = self.model_facing(&call.name);
                        call.name = name;
                    }
                }
                AgentMessage::Tool { name, .. } => {
                    let namespaced = self.model_facing(name);
                    *name = namespaced;
                }
                _ => {}
            }
        }

        let mut result = self.inner.complete_agent_turn(request, options).await?;
        for item in &mut result.items {
            if let AgentTurnItem::ToolCall { call } = item {
                let name = self.canonical(&call.name);
                call.name = name;
            }
        }
        Ok(result)
    }

    fn request_budget_limits(&self) -> RequestBudgetLimits {
        self.inner.request_budget_limits()
    }

    fn token_usage_info(&self) -> TokenUsageInfo {
        self.inner.token_usage_info()
    }

    fn model_name(&self) -> String {
        self.inner.model_name()
    }
}

/// Shape requests the way the OpenCode gateway expects from its own CLI.
///
/// The gateway's free tier only serves requests that stream and declare at
/// least two tools, one named `bash`, so mirror Daat's shell tool under that
/// alias and translate the model's `bash` calls back to the canonical tool.
struct OpenCodeGatewayProvider {
    inner: Box<dyn ModelProvider + Send + Sync>,
}

/// Canonical Daat shell tool the gateway's `bash` alias maps to.
const OPENCODE_GATEWAY_SHELL_TOOL: &str = "terminal__terminal_exec";
/// Canonical Daat file-read tool the gateway's `read` alias maps to.
const OPENCODE_GATEWAY_READ_FILE_TOOL: &str = "read_file";
/// Tool names the gateway's fingerprint check requires.
const OPENCODE_GATEWAY_BASH_TOOL: &str = "bash";
const OPENCODE_GATEWAY_READ_TOOL: &str = "read";

impl OpenCodeGatewayProvider {
    /// Add the CLI tool aliases the gateway's fingerprint check requires.
    ///
    /// The free tier is only served when the request declares both `bash` and
    /// `read` tools (schema contents are not validated). Mirror Daat's shell
    /// and file-read tools under those names.
    fn ensure_cli_tool_aliases(tools: &mut Vec<AgentToolSpec>) {
        let aliases = [
            (
                OPENCODE_GATEWAY_BASH_TOOL,
                OPENCODE_GATEWAY_SHELL_TOOL,
                "Run a shell command.",
            ),
            (
                OPENCODE_GATEWAY_READ_TOOL,
                OPENCODE_GATEWAY_READ_FILE_TOOL,
                "Read a file.",
            ),
        ];
        for (alias_name, canonical_name, description) in aliases {
            if tools.iter().any(|tool| tool.name == alias_name) {
                continue;
            }
            let Some(mut alias) = tools
                .iter()
                .find(|tool| {
                    tool.name == canonical_name
                        || tool.name.ends_with(&format!("_{canonical_name}"))
                })
                .cloned()
            else {
                continue;
            };
            alias.name = alias_name.to_string();
            alias.description = description.to_string();
            tools.push(alias);
        }
    }
}

#[async_trait]
impl ModelProvider for OpenCodeGatewayProvider {
    async fn complete_json(
        &self,
        request: PromptRequest,
        options: ModelRequestOptions,
    ) -> Result<serde_json::Value> {
        self.inner.complete_json(request, options).await
    }

    async fn complete_agent_turn(
        &self,
        mut request: AgentTurnRequest,
        options: ModelRequestOptions,
    ) -> Result<AgentTurnStreamResult> {
        Self::ensure_cli_tool_aliases(&mut request.tools);
        let mut result = self.inner.complete_agent_turn(request, options).await?;
        for item in &mut result.items {
            if let AgentTurnItem::ToolCall { call } = item {
                if call.name == OPENCODE_GATEWAY_BASH_TOOL {
                    call.name = OPENCODE_GATEWAY_SHELL_TOOL.to_string();
                } else if call.name == OPENCODE_GATEWAY_READ_TOOL {
                    call.name = OPENCODE_GATEWAY_READ_FILE_TOOL.to_string();
                }
            }
        }
        Ok(result)
    }

    fn request_budget_limits(&self) -> RequestBudgetLimits {
        self.inner.request_budget_limits()
    }

    fn token_usage_info(&self) -> TokenUsageInfo {
        self.inner.token_usage_info()
    }

    fn model_name(&self) -> String {
        self.inner.model_name()
    }
}

fn repair_dsml_in_stream_result(
    mut result: AgentTurnStreamResult,
    tools: &[AgentToolSpec],
) -> AgentTurnStreamResult {
    let allowed_tool_names: HashSet<String> = tools.iter().map(|t| t.name.clone()).collect();

    let raw_reasoning = result.last_reasoning_content.clone().unwrap_or_default();
    let raw_content = result.last_assistant_message.clone().unwrap_or_default();

    let has_tool_calls = result
        .items
        .iter()
        .any(|item| matches!(item, AgentTurnItem::ToolCall { .. }));

    if !has_tool_calls && !allowed_tool_names.is_empty() {
        let combined = format!("{raw_reasoning}\n{raw_content}");
        let scavenged = dsml_repair::scavenge_dsml_tool_calls(&combined, &allowed_tool_names, 4);
        if !scavenged.is_empty() {
            result.items.extend(
                scavenged
                    .into_iter()
                    .map(|call| AgentTurnItem::ToolCall { call }),
            );
            result.raw_stream_follow_up = true;
        }
    }

    for item in &mut result.items {
        match item {
            AgentTurnItem::ToolCall { call } => {
                dsml_repair::repair_tool_call_arguments(call);
            }
            AgentTurnItem::AssistantMessage { content } => {
                let cleaned = dsml_repair::strip_dsml_from_thinking(content);
                *content = cleaned;
            }
        }
    }

    if let Some(ref rc) = result.last_reasoning_content {
        result.last_reasoning_content = non_empty_string(dsml_repair::strip_dsml_from_thinking(rc));
    }
    if let Some(ref c) = result.last_assistant_message {
        result.last_assistant_message = if c.trim().is_empty() {
            None
        } else {
            Some(dsml_repair::strip_dsml_from_thinking(c))
        };
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ThinkingBudget;
    use crate::reasoning::runtime::{AgentToolCall, HistoryMessage};
    use serde_json::json;

    fn thinking_budget(value: &str) -> ThinkingBudget {
        serde_json::from_value(json!(value)).expect("thinking budget deserializes")
    }

    #[derive(Clone, Default)]
    struct StubProvider {
        seen_tools: Arc<Mutex<Vec<String>>>,
        seen_message_calls: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl ModelProvider for StubProvider {
        async fn complete_json(
            &self,
            _request: PromptRequest,
            _options: ModelRequestOptions,
        ) -> Result<serde_json::Value> {
            Ok(json!({}))
        }

        async fn complete_agent_turn(
            &self,
            request: AgentTurnRequest,
            _options: ModelRequestOptions,
        ) -> Result<AgentTurnStreamResult> {
            *self.seen_tools.lock().unwrap() =
                request.tools.iter().map(|tool| tool.name.clone()).collect();
            for message in &request.messages {
                if let AgentMessage::AssistantToolCallProtocol { calls, .. } = message {
                    for call in calls {
                        self.seen_message_calls
                            .lock()
                            .unwrap()
                            .push(call.name.clone());
                    }
                }
            }
            let name = request
                .tools
                .first()
                .map(|tool| tool.name.clone())
                .unwrap_or_default();
            Ok(AgentTurnStreamResult {
                items: vec![AgentTurnItem::ToolCall {
                    call: AgentToolCall {
                        id: "call-1".to_string(),
                        name,
                        arguments: json!({}),
                    },
                }],
                raw_stream_follow_up: false,
                last_assistant_message: None,
                last_reasoning_content: None,
                last_reasoning_signature: None,
            })
        }

        fn request_budget_limits(&self) -> RequestBudgetLimits {
            RequestBudgetLimits {
                context_window_tokens: 1000,
                auto_compact_threshold_tokens: 900,
                reserved_output_tokens: 100,
            }
        }

        fn token_usage_info(&self) -> TokenUsageInfo {
            TokenUsageInfo::default()
        }

        fn model_name(&self) -> String {
            "stub".to_string()
        }
    }

    #[test]
    fn colliding_model_ids_get_a_tool_name_prefix() {
        assert_eq!(
            model_facing_tool_name_prefix("claude-opus-5-5"),
            Some(MODEL_FACING_TOOL_NAME_PREFIX)
        );
        assert_eq!(
            model_facing_tool_name_prefix("Claude-Sonnet-4-5"),
            Some(MODEL_FACING_TOOL_NAME_PREFIX)
        );
        assert_eq!(model_facing_tool_name_prefix("deepseek-v4.1-flash"), None);
    }

    #[tokio::test]
    async fn prefixed_provider_namespaces_tools_and_restores_call_names() {
        let stub = StubProvider::default();
        let provider = PrefixedToolNameProvider {
            inner: Box::new(stub.clone()),
            prefix: MODEL_FACING_TOOL_NAME_PREFIX,
        };
        let request = AgentTurnRequest {
            messages: vec![
                AgentMessage::user("hi"),
                AgentMessage::assistant_tool_call_protocol_with_reasoning(
                    None,
                    None,
                    vec![AgentToolCall {
                        id: "call-1".to_string(),
                        name: "read_file".to_string(),
                        arguments: json!({}),
                    }],
                ),
                AgentMessage::tool("call-1", "read_file", "contents"),
            ],
            tools: vec![AgentToolSpec {
                name: "read_file".to_string(),
                description: "read a file".to_string(),
                input_spec: AgentToolInputSpec::JsonSchema {
                    schema: json!({"type": "object"}),
                },
            }],
        };
        let options =
            ModelRequestOptions::for_agent_turn(&provider, &request, None).expect("budget options");

        let result = provider
            .complete_agent_turn(request, options)
            .await
            .expect("agent turn");

        assert_eq!(
            *stub.seen_tools.lock().unwrap(),
            vec!["dl_read_file".to_string()]
        );
        assert_eq!(
            *stub.seen_message_calls.lock().unwrap(),
            vec!["dl_read_file".to_string()]
        );
        match &result.items[0] {
            AgentTurnItem::ToolCall { call } => assert_eq!(call.name, "read_file"),
            _ => panic!("expected a tool call item"),
        }
    }

    #[test]
    fn compatible_agent_messages_flatten_unmatched_tool_results() {
        let messages = agent_turn_request_to_openai_messages(
            vec![
                AgentMessage::assistant("assistant tool-call protocol: update_plan"),
                AgentMessage::tool("historical-tool", "historical_tool", "summary=updated plan"),
            ],
            true,
            false,
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
                AgentMessage::assistant_tool_call_protocol_with_reasoning(
                    None,
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
            false,
        );

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1]["role"], "tool");
        assert_eq!(messages[1]["tool_call_id"], "call_123");
    }

    #[test]
    fn compatible_agent_messages_serialize_multimodal_user_content() {
        let dir = tempfile::tempdir().unwrap();
        let image_path = dir.path().join("sample.png");
        std::fs::write(&image_path, b"png-bytes").unwrap();

        let message = agent_message_to_openai_message(
            AgentMessage::user_content(AgentContent::multimodal(
                "describe this",
                vec![AgentContentPart::Image {
                    path: image_path.display().to_string(),
                    media_type: "application/octet-stream".to_string(),
                    description: Some("sample".to_string()),
                }],
            )),
            false,
            false,
        );

        assert_eq!(message["role"], "user");
        assert_eq!(message["content"][0]["type"], "text");
        assert_eq!(message["content"][0]["text"], "describe this");
        assert_eq!(message["content"][1]["type"], "image_url");
        assert!(
            message["content"][1]["image_url"]["url"]
                .as_str()
                .unwrap()
                .starts_with("data:image/png;base64,")
        );
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
                None,
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
    fn openai_host_is_not_matched_inside_path_query_userinfo_or_port() {
        assert!(is_standard_openai_base_url("https://api.openai.com/v1"));
        assert!(is_standard_openai_base_url("https://API.OPENAI.COM"));
        assert!(is_standard_openai_base_url(
            "https://user:pass@api.openai.com:443/v1"
        ));
        assert!(!is_standard_openai_base_url(
            "https://evil.example/api.openai.com"
        ));
        assert!(!is_standard_openai_base_url(
            "https://evil.example/?host=api.openai.com"
        ));
        assert!(!is_standard_openai_base_url(
            "https://api.openai.com.evil.test/v1"
        ));
        assert!(!is_standard_openai_base_url("https://notapi.openai.com/v1"));
    }

    #[test]
    fn opencode_gateway_host_parses_userinfo_port_and_ipv6() {
        assert!(is_opencode_gateway_base_url(
            "https://user:token@api.opencode.ai:8443/zen/v1"
        ));
        assert_eq!(
            opencode_gateway_host("http://[2001:db8::1]:8080/zen").as_deref(),
            Some("[2001:db8::1]")
        );
        assert!(!is_opencode_gateway_base_url(
            "https://evil.example/opencode.ai"
        ));
        assert!(!is_opencode_gateway_base_url(
            "https://[2001:db8::1]/opencode.ai"
        ));
    }

    #[test]
    fn event_stream_content_type_ignores_parameters_and_case() {
        assert!(content_type_is_event_stream(
            "text/event-stream; charset=utf-8"
        ));
        assert!(content_type_is_event_stream("Text/Event-Stream"));
        assert!(!content_type_is_event_stream(
            "application/json; charset=text/event-stream"
        ));
        assert!(!content_type_is_event_stream("text/not-event-stream"));
    }

    #[test]
    fn sse_data_joins_fields_and_ignores_comments() {
        let event = ": keep-alive\nevent: message\ndata: {\"a\":\ndata: 1}\n";
        assert_eq!(sse_event_data(event).as_deref(), Some("{\"a\":\n1}"));
        assert_eq!(
            sse_event_data("data:{\"ok\":true}").as_deref(),
            Some("{\"ok\":true}")
        );
        assert!(sse_event_data(": comment only\nevent: ping").is_none());
    }

    #[test]
    fn fenced_json_block_is_parsed_without_accepting_prose() {
        let fenced = "```json\n{\"ok\":true}\n```";
        assert_eq!(
            extract_json_value_from_content(fenced),
            Some(json!({"ok": true}))
        );
        assert!(extract_json_value_from_content("here is {\"ok\":true} thanks").is_none());
        assert!(extract_json_value_from_content("```json\n{\"ok\":true}").is_none());
    }

    #[test]
    fn opencode_gateway_detection_matches_opencode_ai_host_only() {
        assert!(is_opencode_gateway_base_url("https://opencode.ai/zen/v1"));
        assert!(is_opencode_gateway_base_url("https://api.opencode.ai/v1"));
        assert!(is_opencode_gateway_base_url("http://OPENCODE.AI/zen/v1"));
        assert!(is_opencode_gateway_base_url(
            "http://opencode.ai:8080/zen/v1"
        ));
        assert!(!is_opencode_gateway_base_url("https://api.openai.com/v1"));
        assert!(!is_opencode_gateway_base_url("https://notopencode.ai/v1"));
        assert!(!is_opencode_gateway_base_url(
            "https://opencode.ai.evil.test/v1"
        ));
        assert!(!is_opencode_gateway_base_url(""));
    }

    #[test]
    fn opencode_gateway_headers_carry_cli_identity() {
        let headers = opencode_gateway_headers("https://opencode.ai/zen/v1", Some("session-1"));

        let session = headers.get("x-opencode-session").unwrap().to_str().unwrap();
        assert!(
            session.starts_with("ses_"),
            "unexpected session id {session}"
        );
        assert_eq!(session.len(), 30);
        assert_eq!(
            headers.get("x-opencode-client").unwrap().to_str().unwrap(),
            "cli"
        );
        assert_eq!(
            headers.get("x-opencode-project").unwrap().to_str().unwrap(),
            "global"
        );
        let request = headers.get("x-opencode-request").unwrap().to_str().unwrap();
        assert!(
            request.starts_with("msg_"),
            "unexpected request id {request}"
        );
        assert_eq!(request.len(), 30);
        assert!(
            headers
                .get(reqwest::header::USER_AGENT)
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("opencode/")
        );
        // The same conversation keeps one session id across requests.
        let again = opencode_gateway_headers("https://opencode.ai/zen/v1", Some("session-1"));
        assert_eq!(
            again.get("x-opencode-session"),
            headers.get("x-opencode-session")
        );
    }

    #[test]
    fn opencode_gateway_headers_skip_without_conversation_or_gateway() {
        assert!(
            opencode_gateway_headers("https://opencode.ai/zen/v1", None).is_empty(),
            "missing conversation must not send partial gateway headers"
        );
        assert!(
            opencode_gateway_headers("https://api.openai.com/v1", Some("session-1")).is_empty(),
            "non-gateway providers must not receive opencode headers"
        );
    }

    #[test]
    fn compatible_agent_messages_preserve_out_of_scope_tool_protocol() {
        let messages = agent_turn_request_to_openai_messages(
            vec![AgentMessage::assistant_tool_call_protocol_with_reasoning(
                None,
                None,
                vec![AgentToolCall {
                    id: "call_123".to_string(),
                    name: "terminal_exec".to_string(),
                    arguments: json!({"cmd": "pwd"}),
                }],
            )],
            true,
            false,
            false,
        );

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "assistant");
        assert!(messages[0].get("tool_calls").is_some());
    }

    #[test]
    fn reasoning_content_can_be_forwarded_for_native_tool_protocol() {
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
            false,
        );

        assert_eq!(messages[0]["reasoning_content"], "old reasoning");
        assert_eq!(messages[2]["reasoning_content"], "current reasoning");
    }

    #[test]
    fn reasoning_content_can_be_stripped_after_provider_rejection() {
        let call = AgentToolCall {
            id: "call_old".to_string(),
            name: "update_plan".to_string(),
            arguments: json!({"plan": []}),
        };
        let messages = agent_turn_request_to_openai_messages(
            vec![AgentMessage::assistant_tool_call_protocol_with_reasoning(
                None,
                Some("provider reasoning".to_string()),
                vec![call],
            )],
            false,
            false,
            false,
        );

        assert!(messages[0].get("reasoning_content").is_none());
        assert!(should_retry_request_without_reasoning_content(
            "Bad request: unknown field `reasoning_content` in messages[0]"
        ));
    }

    #[test]
    fn compatible_agent_payload_omits_tool_strict_after_downgrade() {
        let client = OpenAIClient::from_parts(
            "test-key",
            "https://compatible.example/v1",
            &ModelConfig::default(),
        );
        let request = AgentTurnRequest {
            messages: vec![AgentMessage::user("hello")],
            tools: vec![AgentToolSpec {
                name: "demo".to_string(),
                description: "demo tool".to_string(),
                input_spec: AgentToolInputSpec::JsonSchema {
                    schema: json!({
                        "type": "object",
                        "properties": {},
                        "required": [],
                        "additionalProperties": false
                    }),
                },
            }],
        };

        let enabled = CompatibleChatCompletionsAdapter {
            state: ChatCompletionsAdapterState::default(),
        }
        .build_agent_turn_payload(&client, request.clone(), true);
        let omitted = CompatibleChatCompletionsAdapter {
            state: ChatCompletionsAdapterState {
                tool_strict_mode: ToolStrictMode::Omitted,
                ..ChatCompletionsAdapterState::default()
            },
        }
        .build_agent_turn_payload(&client, request, true);

        assert_eq!(enabled["tools"][0]["function"]["strict"], true);
        assert!(omitted["tools"][0]["function"].get("strict").is_none());
    }

    #[test]
    fn compatible_prompt_payload_omits_tool_strict_after_downgrade() {
        let client = OpenAIClient::from_parts(
            "test-key",
            "https://compatible.example/v1",
            &ModelConfig::default(),
        );
        let request = PromptRequest {
            tool_name: "demo".to_string(),
            tool_description: "demo tool".to_string(),
            output_schema: json!({
                "type": "object",
                "properties": {},
                "required": [],
                "additionalProperties": false
            }),
            system_messages: vec![],
            long_term_memory_messages: vec![],
            history_messages: vec![],
            current_user_message: "hello".to_string(),
            retry_messages: vec![],
        };
        let adapter = CompatibleChatCompletionsAdapter {
            state: ChatCompletionsAdapterState {
                tool_strict_mode: ToolStrictMode::Omitted,
                ..ChatCompletionsAdapterState::default()
            },
        };

        let payload =
            adapter.build_prompt_payload(&client, &request, request.output_schema.clone());

        assert!(payload["tools"][0]["function"].get("strict").is_none());
    }

    #[test]
    fn generic_bad_request_downgrades_tool_strict_first() {
        let mut state = ChatCompletionsAdapterState::default();

        assert!(OpenAIClient::step_adapter_state_for_bad_request(
            &mut state, true, true
        ));

        assert_eq!(state.tool_strict_mode, ToolStrictMode::Omitted);
        assert_eq!(
            state.prompt_tool_choice_mode,
            PromptToolChoiceMode::NamedFunction
        );
        assert_eq!(
            state.thinking_budget_mode,
            ThinkingBudgetMode::ReasoningEffortString
        );
    }

    #[test]
    fn standard_openai_bad_request_keeps_tool_strict_enabled() {
        let mut state = ChatCompletionsAdapterState::default();

        assert!(OpenAIClient::step_adapter_state_for_bad_request(
            &mut state, true, false
        ));

        assert_eq!(state.tool_strict_mode, ToolStrictMode::Enabled);
        assert_eq!(
            state.prompt_tool_choice_mode,
            PromptToolChoiceMode::RequiredString
        );
    }

    #[test]
    fn thinking_budget_is_injected_as_reasoning_effort_by_default() {
        let model_config = ModelConfig {
            thinking_budget: Some(thinking_budget("medium")),
            ..Default::default()
        };
        let client = OpenAIClient::from_parts("test-key", "https://api.openai.com", &model_config);

        let payload = build_agent_turn_payload_common(
            &client,
            AgentTurnRequest {
                messages: vec![AgentMessage::user("hello")],
                tools: vec![],
            },
            true,
            false,
            false,
            ToolStrictMode::Enabled,
        );

        assert_eq!(payload["reasoning_effort"], "medium");
    }

    #[test]
    fn deepseek_thinking_budget_uses_thinking_and_reasoning_effort_parameters() {
        let model_config = ModelConfig {
            model_id: "deepseek-reasoner".to_string(),
            thinking_budget: Some(thinking_budget("medium")),
            context_window_tokens: 1_000_000,
            effective_context_window_percent: 100,
            max_completion_tokens: 393_216,
            ..Default::default()
        };
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
            false,
            ToolStrictMode::Enabled,
        );

        assert_eq!(payload["thinking"]["type"], "enabled");
        assert_eq!(payload["reasoning_effort"], "high");
        assert_eq!(payload["max_tokens"], DEEPSEEK_THINKING_MAX_TOKENS);
        assert!(payload.get("reasoning").is_none());
    }

    #[test]
    fn chat_payload_max_tokens_does_not_exceed_reserved_output_budget() {
        let model_config = ModelConfig {
            model_id: "deepseek-v4-flash".to_string(),
            context_window_tokens: 1_000_000,
            effective_context_window_percent: 95,
            max_completion_tokens: 384_000,
            ..Default::default()
        };
        let client =
            OpenAIClient::from_parts("test-key", "https://compatible.example/v1", &model_config);

        let payload = build_agent_turn_payload_common(
            &client,
            AgentTurnRequest {
                messages: vec![AgentMessage::user("hello")],
                tools: vec![],
            },
            true,
            false,
            false,
            ToolStrictMode::Enabled,
        );

        assert_eq!(
            client.request_budget_limits().reserved_output_tokens,
            50_000
        );
        assert_eq!(payload["max_tokens"], 50_000);
    }

    #[test]
    fn chat_payload_and_budget_share_positive_minimum_output_limit() {
        let model_config = ModelConfig {
            context_window_tokens: 200_000,
            effective_context_window_percent: 50,
            ..Default::default()
        };
        assert_eq!(model_config.reserved_output_tokens(), 0);
        let client =
            OpenAIClient::from_parts("test-key", "https://compatible.example/v1", &model_config);

        let payload = build_agent_turn_payload_common(
            &client,
            AgentTurnRequest {
                messages: vec![AgentMessage::user("hello")],
                tools: vec![],
            },
            true,
            false,
            false,
            ToolStrictMode::Enabled,
        );

        assert_eq!(client.request_budget_limits().reserved_output_tokens, 1);
        assert_eq!(payload["max_tokens"], 1);
    }

    #[test]
    fn thinking_budget_can_use_nested_reasoning_payload() {
        let mut payload = json!({
            "model": "demo",
            "messages": [],
        });
        apply_optional_thinking_budget(
            &mut payload,
            Some("xhigh"),
            ThinkingBudgetMode::NestedReasoningObject,
        );

        assert_eq!(payload["reasoning"]["effort"], "xhigh");
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
    #[tokio::test]
    #[ignore = "live OpenCode gateway smoke test; needs the local config and login"]
    async fn live_opencode_console_smoke() {
        let model =
            std::env::var("DL_SMOKE_MODEL").unwrap_or_else(|_| "fledge-alpha-free".to_string());
        let config = crate::config::load_config().await.expect("load config");
        let provider = build_model_provider(&model, &config).expect("build provider");
        let request = AgentTurnRequest {
            messages: vec![AgentMessage::user(
                "Reply with exactly the word PONG and nothing else.",
            )],
            tools: vec![
                AgentToolSpec {
                    name: "terminal__terminal_exec".to_string(),
                    description: "Run a shell command.".to_string(),
                    input_spec: AgentToolInputSpec::JsonSchema {
                        schema: json!({"type": "object"}),
                    },
                },
                AgentToolSpec {
                    name: "read_file".to_string(),
                    description: "Read a file.".to_string(),
                    input_spec: AgentToolInputSpec::JsonSchema {
                        schema: json!({"type": "object"}),
                    },
                },
            ],
        };
        let options = ModelRequestOptions::for_agent_turn(
            provider.as_ref(),
            &request,
            Some("smoke-session".to_string()),
        )
        .expect("budget options");
        let result = provider
            .complete_agent_turn(request, options)
            .await
            .expect("agent turn");
        eprintln!("model={model} items={}", result.items.len());
        for item in &result.items {
            match item {
                AgentTurnItem::AssistantMessage { content } => eprintln!("assistant: {content}"),
                AgentTurnItem::ToolCall { call } => {
                    eprintln!("tool_call: {} {}", call.name, call.arguments);
                }
            }
        }
        assert!(!result.items.is_empty(), "expected a non-empty turn");
    }
}
