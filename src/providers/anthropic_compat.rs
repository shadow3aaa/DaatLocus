//! Anthropic Messages API (`/v1/messages`) compatible provider client.
//!
//! Daat Locus already speaks the OpenAI chat-completions and Responses
//! protocols. Many Anthropic-protocol endpoints (the official Anthropic API and
//! the relays used by Claude Code) expose only the native Messages API, so this
//! module adapts Daat's internal agent-turn and prompt requests to Anthropic
//! Messages requests and parses the Anthropic streaming events back into an
//! [`AgentTurnStreamResult`].
//!
//! It is the dedicated `anthropic-compatible` provider backend
//! (`ProviderConfig::AnthropicCompatible`), not an `openai-compatible`
//! `api_style`.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures_util::StreamExt;
use miette::{Result, miette};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tracing::warn;

use super::io::{
    default_rate_limit_backoff, format_request_error, looks_like_context_window_error,
    looks_like_vision_unsupported_error, non_empty_string, normalize_sse_buffer,
    parse_retry_after_seconds, read_response_text_with_timeout,
    send_request_for_streaming_response, summarize_agent_turn_request, summarize_prompt_request,
    take_next_sse_event, truncate_for_error, truncate_for_json_error,
};
use super::payload::{flatten_tool_result_as_assistant_text, image_part_data_url};
use super::{extract_json_value_from_content, shared_request_rate_limiter};
use crate::context_budget::{ContextBudgetExceededError, RequestBudgetLimits};
use crate::core::{
    ModelProgressSink, ModelProvider, ModelRequestOptions, TokenUsage, TokenUsageInfo,
};
use crate::model_catalog::catalog_model_capacity;
use crate::reasoning::runtime::{
    AgentContent, AgentContentPart, AgentMessage, AgentToolCall, AgentToolInputSpec, AgentToolSpec,
    AgentTurnItem, AgentTurnRequest, AgentTurnStreamResult, PromptRequest,
};

/// Value sent in the `anthropic-version` header. Every Anthropic-compatible
/// relay and the official API accept the stable `2023-06-01` version.
const ANTHROPIC_VERSION: &str = "2023-06-01";
/// Anthropic requires `max_tokens > budget_tokens` and `budget_tokens >= 1024`.
const MIN_THINKING_BUDGET_TOKENS: u64 = 1024;

/// How to ask for extended thinking. Newer models (e.g. Claude Opus 5.x)
/// require adaptive thinking with an `output_config.effort`, while older models
/// only accept an explicit token budget; the client starts in `AUTO` and falls
/// back to the other shape (then disables thinking) on a 400.
const THINKING_MODE_AUTO: u8 = 0;
const THINKING_MODE_ADAPTIVE: u8 = 1;
const THINKING_MODE_BUDGET: u8 = 2;
const THINKING_MODE_DISABLED: u8 = 3;
const MAX_THINKING_RETRIES: u8 = 2;

pub struct AnthropicCompatibleClient {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
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
    request_rate_limiter: Option<Arc<Mutex<VecDeque<Instant>>>>,
    token_usage: std::sync::Mutex<TokenUsageInfo>,
    supports_vision: AtomicBool,
    /// Extended thinking request shape. `AUTO` prefers adaptive thinking and
    /// falls back to an explicit token budget (or off) when a provider rejects
    /// the chosen shape, so thinking keeps working without failing the turn.
    thinking_mode: AtomicU8,
}

impl AnthropicCompatibleClient {
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
            .expect("failed to build anthropic-compatible http client");
        let context_window_tokens = model_config.context_window_tokens();
        let effective_context_window_tokens = model_config.effective_context_window_tokens();
        let auto_compact_threshold_tokens = model_config.auto_compact_token_limit();
        let reserved_output_tokens = model_config.reserved_output_tokens().max(1);
        let max_completion_tokens = model_config.max_completion_tokens();
        let supports_vision = model_config.supports_vision.unwrap_or_else(|| {
            catalog_model_capacity(&model_config.model_id).is_some_and(|c| c.supports_vision)
        });
        Self {
            client,
            api_key: api_key.to_string(),
            base_url: base_url.clone(),
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
            token_usage: std::sync::Mutex::new(TokenUsageInfo {
                total_token_usage: TokenUsage::default(),
                last_token_usage: TokenUsage::default(),
                model_context_window: i64::try_from(context_window_tokens).ok(),
                daily_token_usage: Vec::new(),
            }),
            supports_vision: AtomicBool::new(supports_vision),
            thinking_mode: AtomicU8::new(THINKING_MODE_AUTO),
        }
    }

    fn url(&self) -> String {
        anthropic_messages_url(&self.base_url)
    }

    /// Output cap for one request. Anthropic requires `max_tokens` on every
    /// call, so mirror the chat-completions budget: the smaller of the
    /// configured completion limit and the reserved output window.
    fn max_output_tokens(&self) -> u64 {
        u64::try_from(
            self.max_completion_tokens
                .min(self.reserved_output_tokens)
                .max(1),
        )
        .unwrap_or(4096)
    }

    const fn request_budget_limits(&self) -> RequestBudgetLimits {
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
        let window = Duration::from_mins(1);
        loop {
            let mut queue = limiter.lock().await;
            let now = Instant::now();
            queue.retain(|t| now.duration_since(*t) < window);
            if queue.len() < rpm {
                queue.push_back(now);
                return;
            }
            let oldest = *queue.front().unwrap();
            let wait = window.checked_sub(now.duration_since(oldest)).unwrap();
            drop(queue);
            tokio::time::sleep(wait).await;
        }
    }

    async fn post_messages_with_retry(
        &self,
        payload: &Value,
        request_context: &[String],
        session_headers: &reqwest::header::HeaderMap,
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
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", ANTHROPIC_VERSION)
                .header(
                    reqwest::header::AUTHORIZATION,
                    format!("Bearer {}", self.api_key),
                )
                .headers(session_headers.clone())
                .json(payload);
            let response = send_request_for_streaming_response(
                request,
                self.request_timeout,
                "anthropic-compatible request failed",
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
                    "anthropic-compatible 429 body read failed",
                    &url,
                    request_context,
                )
                .await?;

                if rate_limit_attempt >= MAX_429_RETRIES {
                    return Err(miette!(
                        "anthropic-compatible returned HTTP 429 after {} retries: {}",
                        MAX_429_RETRIES,
                        truncate_for_error(&body)
                    ));
                }

                let delay = retry_after.map_or_else(
                    || default_rate_limit_backoff(rate_limit_attempt),
                    Duration::from_secs,
                );
                warn!(
                    "anthropic-compatible returned HTTP 429; retrying in {} ms (attempt {}/{})\n{}",
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
                    "anthropic-compatible 5xx body read failed",
                    &url,
                    request_context,
                )
                .await?;

                if transient_attempt >= MAX_5XX_RETRIES {
                    return Err(miette!(
                        "anthropic-compatible returned HTTP {} after {} retries: {}",
                        status,
                        MAX_5XX_RETRIES,
                        truncate_for_error(&body)
                    ));
                }

                let delay = Duration::from_millis(400 * (1u64 << transient_attempt));
                warn!(
                    "anthropic-compatible returned HTTP {}; retrying in {} ms (attempt {}/{})\n{}",
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
            info.model_context_window = i64::try_from(self.context_window_tokens).ok();
            info.append_last_usage(usage);
        }
    }

    async fn parse_messages_stream(
        &self,
        progress: Option<&ModelProgressSink>,
        response: reqwest::Response,
        emit_progress: bool,
    ) -> Result<AgentTurnStreamResult> {
        let url = self.url();
        let mut buffer = Vec::new();
        let mut state = AnthropicStreamState::default();
        let mut stream = response.bytes_stream();
        let stream_request_context = [
            format!("model={}", self.model),
            "phase=message_stream".to_string(),
        ];

        while !state.completed {
            let next_chunk = tokio::time::timeout(self.stream_idle_timeout, stream.next())
                .await
                .map_err(|_| {
                    miette!(
                        "anthropic-compatible stream stalled for over {}s (model={}, url={})",
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
                    "anthropic-compatible stream read failed",
                    &url,
                    &stream_request_context,
                    &err,
                )
            })?;
            buffer.extend_from_slice(&chunk);
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
                    state.completed = true;
                    break;
                }
                let Ok(value) = serde_json::from_str::<Value>(&data) else {
                    // Anthropic sends an `event:` line alongside `data:`; keep
                    // tolerating any non-JSON keepalive payloads.
                    continue;
                };
                state.apply_event(&value)?;
                if emit_progress {
                    state.emit_progress(progress);
                }
            }
        }

        if emit_progress {
            state.emit_progress(progress);
        }

        let usage = state.token_usage();
        if let Some(usage) = usage {
            self.record_last_usage(usage);
        }
        Ok(state.into_stream_result())
    }
}

#[async_trait]
impl ModelProvider for AnthropicCompatibleClient {
    async fn complete_json(
        &self,
        request: PromptRequest,
        options: ModelRequestOptions,
    ) -> Result<Value> {
        let budget = &options.budget;
        let request_context = summarize_prompt_request(&request, Some(budget));
        let session_headers =
            super::opencode_gateway_headers(&self.base_url, options.conversation_id.as_deref());
        let tool_name = request.tool_name.clone();
        // Some models (e.g. Claude Opus 5.5) reject a forced tool_choice; fall back
        // to `auto` and read the structured object from the text answer.
        let mut force_tool = true;
        let response = loop {
            let payload = build_prompt_payload(self, &request, force_tool);
            let response = self
                .post_messages_with_retry(&payload, &request_context, &session_headers)
                .await?;
            let status = response.status();
            if status.is_success() {
                break response;
            }
            let url = self.url();
            let body = read_response_text_with_timeout(
                response,
                self.request_timeout,
                "anthropic-compatible body read failed",
                &url,
                &request_context,
            )
            .await?;
            if force_tool && looks_like_forced_tool_choice_error(&body) {
                force_tool = false;
                warn!(
                    "anthropic-compatible rejected forced tool_choice; retrying with tool_choice=auto\n{}",
                    request_context.join("\n")
                );
                continue;
            }
            if looks_like_anthropic_context_error(&body) {
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
            return Err(miette!(
                "anthropic-compatible returned HTTP {}: {}",
                status,
                truncate_for_error(&body)
            ));
        };
        let result = self
            .parse_messages_stream(options.progress.as_ref(), response, false)
            .await?;
        if let Some(call) = result.items.iter().find_map(|item| match item {
            AgentTurnItem::ToolCall { call } if call.name == tool_name => Some(call.clone()),
            _ => None,
        }) {
            return Ok(call.arguments);
        }
        let content = result.last_assistant_message.as_deref().unwrap_or_default();
        if let Some(value) = extract_json_value_from_content(content) {
            return Ok(value);
        }
        Err(miette!(
            "anthropic-compatible JSON request did not return a structured object; content={}",
            truncate_for_error(content)
        ))
    }

    async fn complete_agent_turn(
        &self,
        request: AgentTurnRequest,
        options: ModelRequestOptions,
    ) -> Result<AgentTurnStreamResult> {
        let budget = &options.budget;
        let request_context = summarize_agent_turn_request(&request, Some(budget));
        let session_headers =
            super::opencode_gateway_headers(&self.base_url, options.conversation_id.as_deref());
        let mut strip_images = !self.supports_vision.load(Ordering::Relaxed);
        let mut thinking_retries = 0u8;
        loop {
            let payload = build_agent_payload(self, request.clone(), strip_images);
            let response = self
                .post_messages_with_retry(&payload, &request_context, &session_headers)
                .await?;
            let status = response.status();
            if status.is_success() {
                return self
                    .parse_messages_stream(options.progress.as_ref(), response, true)
                    .await;
            }
            let url = self.url();
            let body = read_response_text_with_timeout(
                response,
                self.request_timeout,
                "anthropic-compatible body read failed",
                &url,
                &request_context,
            )
            .await?;
            if looks_like_anthropic_context_error(&body) {
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
            let current_thinking_mode = self.thinking_mode.load(Ordering::Relaxed);
            let adaptive_available = self
                .thinking_budget
                .as_deref()
                .is_some_and(|budget| adaptive_effort(budget).is_some());
            if thinking_retries < MAX_THINKING_RETRIES
                && let Some(next_mode) =
                    next_thinking_mode_on_error(&body, current_thinking_mode, adaptive_available)
                && next_mode != current_thinking_mode
            {
                self.thinking_mode.store(next_mode, Ordering::Relaxed);
                thinking_retries += 1;
                warn!(
                    "anthropic-compatible rejected thinking_mode={} ; retrying with thinking_mode={}\n{}",
                    current_thinking_mode,
                    next_mode,
                    request_context.join("\n")
                );
                continue;
            }
            if looks_like_vision_unsupported_error(&body)
                && self.supports_vision.load(Ordering::Relaxed)
            {
                self.supports_vision.store(false, Ordering::Relaxed);
                strip_images = true;
                warn!(
                    "anthropic-compatible rejected image input; retrying without images\n{}",
                    request_context.join("\n")
                );
                continue;
            }
            return Err(miette!(
                "anthropic-compatible returned HTTP {}: {}",
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
// URL helpers
// ---------------------------------------------------------------------------

/// Resolve the Anthropic Messages endpoint from a configured base URL.
///
/// Daat's convention is that a base URL already includes the API version
/// segment (`https://host/v1`), while Anthropic's own convention appends
/// `/v1`. Accept both: a base already ending in `/v1` (or pointing straight at
/// `/messages`) is used as-is, and anything else gets `/v1/messages` appended.
fn anthropic_messages_url(base_url: &str) -> String {
    let base = base_url.trim().trim_end_matches('/');
    if base.ends_with("/messages") {
        return base.to_string();
    }
    if base.ends_with("/v1") {
        return format!("{base}/messages");
    }
    format!("{base}/v1/messages")
}

fn looks_like_anthropic_context_error(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    looks_like_context_window_error(&lower)
        || lower.contains("prompt is too long")
        || lower.contains("input length and `max_tokens` exceed")
}

// ---------------------------------------------------------------------------
// Request payload construction
// ---------------------------------------------------------------------------

fn build_agent_payload(
    client: &AnthropicCompatibleClient,
    request: AgentTurnRequest,
    strip_images: bool,
) -> Value {
    let include_reasoning_blocks = thinking_enabled_for_request(client);
    let (system, messages) =
        agent_messages_to_anthropic(request.messages, strip_images, include_reasoning_blocks);
    let mut payload = json!({
        "model": client.model,
        "max_tokens": client.max_output_tokens(),
        "messages": messages,
        "stream": true,
    });
    if let Some(system) = system {
        payload["system"] = json!(system);
    }

    let tools = request
        .tools
        .into_iter()
        .map(agent_tool_to_anthropic_tool)
        .collect::<Vec<_>>();
    if !tools.is_empty() {
        payload["tools"] = json!(tools);
        payload["tool_choice"] = json!({ "type": "auto" });
    }

    apply_thinking(client, &mut payload);
    apply_temperature(client, &mut payload);
    payload
}

fn build_prompt_payload(
    client: &AnthropicCompatibleClient,
    request: &PromptRequest,
    force_tool: bool,
) -> Value {
    let (system, messages) = agent_messages_to_anthropic(
        collect_prompt_messages(request),
        !client.supports_vision.load(Ordering::Relaxed),
        false,
    );
    // The OpenAI paths force a structured-output tool call; the Anthropic-native
    // equivalent is a single tool with the output schema forced via tool_choice.
    // A forced tool_choice is incompatible with extended thinking, so prompt
    // requests keep normal sampling.
    let mut payload = json!({
        "model": client.model,
        "max_tokens": client.max_output_tokens(),
        "messages": messages,
        "tools": [{
            "name": request.tool_name,
            "description": request.tool_description,
            "input_schema": request.output_schema,
        }],
        "tool_choice": if force_tool {
            json!({ "type": "tool", "name": request.tool_name })
        } else {
            json!({ "type": "auto" })
        },
        "stream": true,
    });
    if let Some(system) = system {
        payload["system"] = json!(system);
    }
    apply_temperature(client, &mut payload);
    payload
}

fn collect_prompt_messages(request: &PromptRequest) -> Vec<AgentMessage> {
    request
        .all_messages()
        .into_iter()
        .map(|history| history.message)
        .collect()
}

/// Add a non-default `temperature` only when thinking is not configured: models
/// with always-on adaptive thinking (Claude Opus 5.x) reject non-default
/// sampling parameters, and omitting temperature is always safe.
fn apply_temperature(client: &AnthropicCompatibleClient, payload: &mut Value) {
    if !thinking_configured(client) {
        payload["temperature"] = json!(client.temperature);
    }
}

/// A 400 that means the provider rejects a forced `tool_choice`.
fn looks_like_forced_tool_choice_error(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    lower.contains("tool_choice")
        && (lower.contains("does not support")
            || lower.contains("unsupported")
            || lower.contains("not supported")
            || lower.contains("invalid"))
}

/// Whether extended thinking is configured at all (any non-`none` budget).
fn thinking_configured(client: &AnthropicCompatibleClient) -> bool {
    client
        .thinking_budget
        .as_deref()
        .is_some_and(|budget| !is_thinking_disabled(budget))
}

fn is_thinking_disabled(budget: &str) -> bool {
    matches!(
        budget.trim().to_ascii_lowercase().as_str(),
        "none" | "off" | "disabled" | "false" | ""
    )
}

/// Whether reasoning blocks should be replayed and thinking requested. Once a
/// provider has rejected every thinking shape, this turns off so later turns
/// skip thinking entirely.
fn thinking_enabled_for_request(client: &AnthropicCompatibleClient) -> bool {
    thinking_configured(client)
        && client.thinking_mode.load(Ordering::Relaxed) != THINKING_MODE_DISABLED
}

/// Adaptive-thinking effort derived from the provider-agnostic budget.
/// `claude-opus-5-5` supports the five levels `low`, `medium`, `high`, `xhigh`
/// and `max` (default `medium`); numeric budgets map to the nearest level.
fn adaptive_effort(budget: &str) -> Option<&'static str> {
    let normalized = budget.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "minimal" | "low" => return Some("low"),
        "medium" => return Some("medium"),
        "high" => return Some("high"),
        "xhigh" => return Some("xhigh"),
        "max" | "ultra" => return Some("max"),
        _ => {}
    }
    let tokens = normalized.parse::<u64>().ok()?;
    Some(match tokens {
        0..4096 => "low",
        4096..16384 => "medium",
        16384..32768 => "high",
        32768..65536 => "xhigh",
        _ => "max",
    })
}

/// Explicit token budget for models that only accept `enabled` thinking.
fn explicit_thinking_budget(budget: &str) -> Option<u64> {
    let normalized = budget.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "minimal" | "low" => return Some(2048),
        "medium" => return Some(8192),
        "high" => return Some(16384),
        "xhigh" | "max" | "ultra" => return Some(24576),
        _ => {}
    }
    normalized
        .parse::<u64>()
        .ok()
        .filter(|tokens| *tokens >= MIN_THINKING_BUDGET_TOKENS)
}

/// Apply the configured extended thinking when the trailing turn allows it.
/// Returns `true` when thinking was enabled (in which case sampling parameters
/// must be omitted).
fn apply_thinking(client: &AnthropicCompatibleClient, payload: &mut Value) -> bool {
    if !thinking_enabled_for_request(client) {
        return false;
    }
    let Some(budget) = client.thinking_budget.as_deref() else {
        return false;
    };
    let Some(messages) = payload.get("messages").and_then(Value::as_array) else {
        return false;
    };
    let prefer_adaptive = match client.thinking_mode.load(Ordering::Relaxed) {
        THINKING_MODE_ADAPTIVE => true,
        THINKING_MODE_BUDGET => false,
        // AUTO: prefer adaptive whenever the budget maps to an effort.
        _ => adaptive_effort(budget).is_some(),
    };
    // Adaptive thinking lets the model skip thinking on any step, and the API
    // accepts adaptive thinking after an assistant tool_use turn that carried no
    // thinking block. Only the legacy `enabled` shape needs the preceding
    // assistant turn to replay a signed thinking block (see
    // [`trailing_turn_supports_thinking`]); gating adaptive on it would switch
    // thinking off for the rest of a tool loop after the first skipped step.
    if !prefer_adaptive && !trailing_turn_supports_thinking(messages) {
        return false;
    }
    if prefer_adaptive && let Some(effort) = adaptive_effort(budget) {
        payload["thinking"] = json!({ "type": "adaptive" });
        payload["output_config"] = json!({ "effort": effort });
        return true;
    }
    let Some(budget_tokens) = explicit_thinking_budget(budget) else {
        return false;
    };
    let max_tokens = payload
        .get("max_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let budget_tokens = budget_tokens.min(max_tokens / 2);
    if budget_tokens < MIN_THINKING_BUDGET_TOKENS {
        return false;
    }
    payload["thinking"] = json!({ "type": "enabled", "budget_tokens": budget_tokens });
    true
}

/// Next thinking shape to try after a 400 that mentions thinking, or `None`
/// when the error is unrelated to thinking.
fn next_thinking_mode_on_error(body: &str, current: u8, adaptive_available: bool) -> Option<u8> {
    let lower = body.to_ascii_lowercase();
    if !lower.contains("thinking") {
        return None;
    }
    // "requires adaptive thinking" (or "must"/"only") means the provider
    // demands adaptive; any other mention of adaptive means it was rejected.
    let requires_adaptive = lower.contains("adaptive")
        && (lower.contains("require") || lower.contains("must") || lower.contains("only"));
    if requires_adaptive {
        // The provider only accepts adaptive thinking. When that is already what
        // we send there is no other shape to try, and downgrading would only
        // disable thinking for the rest of the process, so surface the error.
        return (adaptive_available && current != THINKING_MODE_ADAPTIVE)
            .then_some(THINKING_MODE_ADAPTIVE);
    }
    let next = if current != THINKING_MODE_BUDGET {
        THINKING_MODE_BUDGET
    } else {
        THINKING_MODE_DISABLED
    };
    Some(next)
}

/// Anthropic only accepts a thinking continuation when the assistant turn that
/// precedes the trailing tool result still carries its signed thinking block,
/// which Daat replays from history. A fresh user prompt is always compatible.
fn trailing_turn_supports_thinking(messages: &[Value]) -> bool {
    let Some(last) = messages.last() else {
        return true;
    };
    if last.get("role").and_then(Value::as_str) != Some("user") {
        return false;
    }
    let has_tool_result = last
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|block| block.get("type").and_then(Value::as_str) == Some("tool_result"));
    if !has_tool_result {
        return true;
    }
    messages[..messages.len() - 1]
        .iter()
        .rev()
        .find(|message| message.get("role").and_then(Value::as_str) == Some("assistant"))
        .and_then(|assistant| assistant.get("content").and_then(Value::as_array))
        .is_some_and(|blocks| {
            blocks.iter().any(|block| {
                matches!(
                    block.get("type").and_then(Value::as_str),
                    Some("thinking" | "redacted_thinking")
                )
            })
        })
}

fn agent_tool_to_anthropic_tool(tool: AgentToolSpec) -> Value {
    match tool.input_spec {
        AgentToolInputSpec::JsonSchema { schema } => json!({
            "name": tool.name,
            "description": tool.description,
            "input_schema": schema,
        }),
        AgentToolInputSpec::FreeformGrammar {
            syntax,
            definition,
            fallback_schema,
        } => json!({
            "name": tool.name,
            "description": format!(
                "{}\n\nThis is a FREEFORM grammar tool. Put the complete tool input in the `input` field.\nsyntax={syntax}\ndefinition=\n{definition}",
                tool.description
            ),
            "input_schema": fallback_schema,
        }),
    }
}

fn empty_user_message() -> Value {
    json!({
        "role": "user",
        "content": [{ "type": "text", "text": "(continuing the conversation)" }],
    })
}

// ---------------------------------------------------------------------------
// Internal messages -> Anthropic messages
// ---------------------------------------------------------------------------

fn agent_messages_to_anthropic(
    messages: Vec<AgentMessage>,
    strip_images: bool,
    include_reasoning_blocks: bool,
) -> (Option<String>, Vec<Value>) {
    let mut system_parts: Vec<String> = Vec::new();
    let mut anthropic_messages: Vec<Value> = Vec::new();
    let mut valid_tool_call_ids: std::collections::HashSet<String> =
        std::collections::HashSet::new();

    for message in messages {
        match message {
            AgentMessage::System { content } => {
                if !content.trim().is_empty() {
                    system_parts.push(content);
                }
            }
            AgentMessage::User { content } => {
                push_message_blocks(
                    &mut anthropic_messages,
                    "user",
                    anthropic_user_blocks(&content, strip_images),
                );
            }
            AgentMessage::Assistant { content } => {
                if !content.trim().is_empty() {
                    push_message_blocks(
                        &mut anthropic_messages,
                        "assistant",
                        vec![json!({ "type": "text", "text": content })],
                    );
                }
            }
            AgentMessage::AssistantToolCallProtocol {
                content,
                reasoning_content,
                reasoning_signature,
                calls,
            } => {
                let mut blocks = Vec::new();
                // Replay the signed thinking block first: Anthropic requires the
                // assistant turn that produced a tool_use to carry its thinking
                // block verbatim, otherwise a thinking continuation is rejected.
                // With `thinking.display = "omitted"` (the default for Opus 5.x)
                // the thinking text is empty, so the signature alone is enough.
                if include_reasoning_blocks
                    && let Some(signature) = reasoning_signature
                        .as_deref()
                        .filter(|signature| !signature.trim().is_empty())
                {
                    blocks.push(json!({
                        "type": "thinking",
                        "thinking": reasoning_content.as_deref().unwrap_or(""),
                        "signature": signature,
                    }));
                }
                if let Some(text) = content.filter(|text| !text.trim().is_empty()) {
                    blocks.push(json!({ "type": "text", "text": text }));
                }
                for call in calls {
                    valid_tool_call_ids.insert(call.id.clone());
                    blocks.push(json!({
                        "type": "tool_use",
                        "id": call.id,
                        "name": call.name,
                        "input": tool_input_object(&call.arguments),
                    }));
                }
                push_message_blocks(&mut anthropic_messages, "assistant", blocks);
            }
            AgentMessage::Tool {
                tool_call_id,
                name,
                content,
            } => {
                if valid_tool_call_ids.contains(&tool_call_id) {
                    push_tool_result_block(
                        &mut anthropic_messages,
                        json!({
                            "type": "tool_result",
                            "tool_use_id": tool_call_id,
                            "content": content,
                        }),
                    );
                } else if !content.trim().is_empty() {
                    push_message_blocks(
                        &mut anthropic_messages,
                        "user",
                        vec![json!({
                            "type": "text",
                            "text": flatten_tool_result_as_assistant_text(&name, &content),
                        })],
                    );
                }
            }
        }
    }

    drop_incomplete_tool_turns(&mut anthropic_messages);
    drop_empty_messages(&mut anthropic_messages);
    ensure_leading_user_message(&mut anthropic_messages);

    let system = non_empty_string(system_parts.join("\n\n"));
    (system, anthropic_messages)
}

fn tool_input_object(arguments: &Value) -> Value {
    if arguments.is_object() {
        arguments.clone()
    } else {
        json!({ "input": arguments })
    }
}

fn anthropic_user_blocks(content: &AgentContent, strip_images: bool) -> Vec<Value> {
    let mut blocks = Vec::new();
    if !content.as_text().trim().is_empty() {
        blocks.push(json!({ "type": "text", "text": content.as_text() }));
    }
    for part in content.parts() {
        match part {
            AgentContentPart::Text { text } => {
                if !text.trim().is_empty() {
                    blocks.push(json!({ "type": "text", "text": text }));
                }
            }
            AgentContentPart::Image {
                path, description, ..
            } => {
                if strip_images {
                    blocks.push(json!({
                        "type": "text",
                        "text": format!("[image: {}]", description.as_deref().unwrap_or(path)),
                    }));
                    continue;
                }
                let Some(url) = image_part_data_url(part) else {
                    blocks.push(json!({
                        "type": "text",
                        "text": format!(
                            "[image attachment unavailable: {}]",
                            description.as_deref().unwrap_or(path)
                        ),
                    }));
                    continue;
                };
                if let Some(block) = anthropic_image_block_from_data_url(&url) {
                    blocks.push(block);
                } else {
                    blocks.push(json!({
                        "type": "text",
                        "text": format!(
                            "[image attachment unavailable: {}]",
                            description.as_deref().unwrap_or(path)
                        ),
                    }));
                }
            }
        }
    }
    blocks
}

/// Convert a `data:<media-type>;base64,<data>` URL into an Anthropic image
/// content block.
fn anthropic_image_block_from_data_url(data_url: &str) -> Option<Value> {
    let rest = data_url.strip_prefix("data:")?;
    let (meta, data) = rest.split_once(',')?;
    if !meta.contains("base64") || data.is_empty() {
        return None;
    }
    let media_type = meta.split(';').next()?.trim();
    if !media_type.starts_with("image/") {
        return None;
    }
    Some(json!({
        "type": "image",
        "source": {
            "type": "base64",
            "media_type": media_type,
            "data": data,
        },
    }))
}

/// Append blocks to the trailing message when it already has the same role,
/// otherwise start a new message. Anthropic requires alternating roles, so
/// consecutive same-role messages must be merged.
fn push_message_blocks(messages: &mut Vec<Value>, role: &str, blocks: Vec<Value>) {
    if blocks.is_empty() {
        return;
    }
    if let Some(last) = messages.last_mut()
        && last.get("role").and_then(Value::as_str) == Some(role)
        && let Some(content) = last.get_mut("content").and_then(Value::as_array_mut)
    {
        content.extend(blocks);
        return;
    }
    messages.push(json!({ "role": role, "content": blocks }));
}

/// Append a tool result while preserving Anthropic's ordering rule: every
/// `tool_result` block must precede any text or image block in the user turn.
fn push_tool_result_block(messages: &mut Vec<Value>, block: Value) {
    if let Some(last) = messages.last_mut()
        && last.get("role").and_then(Value::as_str) == Some("user")
        && let Some(content) = last.get_mut("content").and_then(Value::as_array_mut)
    {
        let insert_at = content
            .iter()
            .position(|item| item.get("type").and_then(Value::as_str) != Some("tool_result"))
            .unwrap_or(content.len());
        content.insert(insert_at, block);
        return;
    }
    messages.push(json!({ "role": "user", "content": [block] }));
}

/// Drop tool turns that no longer form a complete adjacent assistant
/// `tool_use` / user `tool_result` pair (a compacted or resumed history can
/// break the pairing, and Anthropic rejects such turns).
fn drop_incomplete_tool_turns(messages: &mut Vec<Value>) {
    let original = std::mem::take(messages);
    let mut sanitized = Vec::with_capacity(original.len());
    let mut index = 0;

    while index < original.len() {
        let message = &original[index];
        let is_assistant = message.get("role").and_then(Value::as_str) == Some("assistant");
        let tool_use_ids = if is_assistant {
            message_block_ids(message, "tool_use", "id")
        } else {
            Vec::new()
        };

        if !tool_use_ids.is_empty() {
            let paired_user = original
                .get(index + 1)
                .filter(|next| next.get("role").and_then(Value::as_str) == Some("user"));
            let tool_result_ids = paired_user
                .map(|user| message_block_ids(user, "tool_result", "tool_use_id"))
                .unwrap_or_default();
            let unique_tool_uses: std::collections::HashSet<&str> =
                tool_use_ids.iter().copied().collect();
            let unique_tool_results: std::collections::HashSet<&str> =
                tool_result_ids.iter().copied().collect();
            let complete = tool_use_ids.iter().all(|id| !id.is_empty())
                && tool_result_ids.iter().all(|id| !id.is_empty())
                && unique_tool_uses.len() == tool_use_ids.len()
                && unique_tool_results.len() == tool_result_ids.len()
                && unique_tool_uses == unique_tool_results;

            if complete {
                sanitized.push(message.clone());
                sanitized.push(paired_user.unwrap().clone());
            } else if let Some(user) = paired_user {
                let mut user = user.clone();
                drop_tool_result_blocks(&mut user);
                if message_has_content(&user) {
                    sanitized.push(user);
                }
            }

            index += if paired_user.is_some() { 2 } else { 1 };
            continue;
        }

        let mut message = message.clone();
        if message.get("role").and_then(Value::as_str) == Some("user") {
            drop_tool_result_blocks(&mut message);
        }
        if message_has_content(&message) {
            sanitized.push(message);
        }
        index += 1;
    }

    *messages = sanitized;
}

fn message_block_ids<'a>(message: &'a Value, block_type: &str, id_field: &str) -> Vec<&'a str> {
    message
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some(block_type))
        .map(|block| block.get(id_field).and_then(Value::as_str).unwrap_or(""))
        .collect()
}

fn drop_tool_result_blocks(message: &mut Value) {
    if let Some(content) = message.get_mut("content").and_then(Value::as_array_mut) {
        content.retain(|block| block.get("type").and_then(Value::as_str) != Some("tool_result"));
    }
}

fn drop_empty_messages(messages: &mut Vec<Value>) {
    messages.retain(message_has_content);
}

fn message_has_content(message: &Value) -> bool {
    message
        .get("content")
        .and_then(Value::as_array)
        .map(|content| !content.is_empty())
        .unwrap_or(true)
}

fn ensure_leading_user_message(messages: &mut Vec<Value>) {
    let leads_with_user = messages
        .first()
        .and_then(|message| message.get("role"))
        .and_then(Value::as_str)
        == Some("user");
    if !leads_with_user {
        messages.insert(0, empty_user_message());
    }
}

// ---------------------------------------------------------------------------
// Anthropic streaming events
// ---------------------------------------------------------------------------

#[derive(Default, Clone)]
struct AnthropicBlock {
    kind: String,
    text: String,
    thinking: String,
    signature: String,
    tool_id: String,
    tool_name: String,
    input_start: Value,
    partial_json: String,
}

#[derive(Default)]
struct AnthropicStreamState {
    blocks: BTreeMap<u64, AnthropicBlock>,
    input_tokens: i64,
    cache_read_input_tokens: i64,
    cache_creation_input_tokens: i64,
    output_tokens: i64,
    completed: bool,
    last_assistant_progress_emit_at: Option<Instant>,
    last_assistant_progress_char_len: usize,
    last_reasoning_progress_emit_at: Option<Instant>,
    last_reasoning_progress_char_len: usize,
}

impl AnthropicStreamState {
    fn apply_event(&mut self, value: &Value) -> Result<()> {
        match value.get("type").and_then(Value::as_str).unwrap_or("") {
            "message_start" => {
                if let Some(usage) = value.pointer("/message/usage") {
                    self.input_tokens = usage
                        .get("input_tokens")
                        .and_then(Value::as_i64)
                        .unwrap_or(self.input_tokens);
                    self.cache_read_input_tokens = usage
                        .get("cache_read_input_tokens")
                        .and_then(Value::as_i64)
                        .unwrap_or(self.cache_read_input_tokens);
                    self.cache_creation_input_tokens = usage
                        .get("cache_creation_input_tokens")
                        .and_then(Value::as_i64)
                        .unwrap_or(self.cache_creation_input_tokens);
                }
            }
            "content_block_start" => {
                if let Some(index) = value.get("index").and_then(Value::as_u64) {
                    let block = value.get("content_block").cloned().unwrap_or_default();
                    let entry = self.blocks.entry(index).or_default();
                    entry.kind = block
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    entry.tool_id = block
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    entry.tool_name = block
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    entry.input_start = block.get("input").cloned().unwrap_or_else(|| json!({}));
                    if let Some(signature) = block.get("signature").and_then(Value::as_str) {
                        entry.signature.push_str(signature);
                    }
                }
            }
            "content_block_delta" => {
                if let Some(index) = value.get("index").and_then(Value::as_u64) {
                    let delta = value.get("delta").cloned().unwrap_or_default();
                    let entry = self.blocks.entry(index).or_default();
                    match delta.get("type").and_then(Value::as_str).unwrap_or("") {
                        "text_delta" => {
                            if let Some(text) = delta.get("text").and_then(Value::as_str) {
                                entry.text.push_str(text);
                            }
                        }
                        "thinking_delta" => {
                            if let Some(text) = delta.get("thinking").and_then(Value::as_str) {
                                entry.thinking.push_str(text);
                            }
                        }
                        "input_json_delta" => {
                            if let Some(partial) = delta.get("partial_json").and_then(Value::as_str)
                            {
                                entry.partial_json.push_str(partial);
                            }
                        }
                        "signature_delta" => {
                            if let Some(signature) = delta.get("signature").and_then(Value::as_str)
                            {
                                entry.signature.push_str(signature);
                            }
                        }
                        _ => {}
                    }
                }
            }
            "message_delta" => {
                if let Some(usage) = value.get("usage") {
                    self.output_tokens = usage
                        .get("output_tokens")
                        .and_then(Value::as_i64)
                        .unwrap_or(self.output_tokens);
                    self.input_tokens = usage
                        .get("input_tokens")
                        .and_then(Value::as_i64)
                        .unwrap_or(self.input_tokens);
                }
            }
            "message_stop" => self.completed = true,
            "error" => {
                return Err(miette!(
                    "anthropic-compatible stream failed: {}",
                    truncate_for_json_error(value)
                ));
            }
            _ => {}
        }
        Ok(())
    }

    fn assistant_text(&self) -> String {
        self.blocks
            .values()
            .filter(|block| block.kind == "text")
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>()
            .join("")
    }

    fn reasoning_text(&self) -> String {
        self.blocks
            .values()
            .filter(|block| block.kind == "thinking" || block.kind == "redacted_thinking")
            .map(|block| block.thinking.as_str())
            .collect::<Vec<_>>()
            .join("")
    }

    fn emit_progress(&mut self, progress: Option<&ModelProgressSink>) {
        let Some(progress) = progress else {
            return;
        };
        let text_len = self.assistant_text().chars().count();
        let should_emit_text = text_len > self.last_assistant_progress_char_len
            && (text_len.saturating_sub(self.last_assistant_progress_char_len) >= 64
                || self
                    .last_assistant_progress_emit_at
                    .is_none_or(|at| at.elapsed() >= Duration::from_millis(800)));
        if should_emit_text {
            progress.emit_assistant_content(self.assistant_text());
            self.last_assistant_progress_emit_at = Some(Instant::now());
            self.last_assistant_progress_char_len = text_len;
        }
        let reasoning_len = self.reasoning_text().chars().count();
        let should_emit_reasoning = reasoning_len > self.last_reasoning_progress_char_len
            && (reasoning_len.saturating_sub(self.last_reasoning_progress_char_len) >= 64
                || self
                    .last_reasoning_progress_emit_at
                    .is_none_or(|at| at.elapsed() >= Duration::from_millis(800)));
        if should_emit_reasoning {
            progress.emit_reasoning_content(self.reasoning_text());
            self.last_reasoning_progress_emit_at = Some(Instant::now());
            self.last_reasoning_progress_char_len = reasoning_len;
        }
    }

    fn token_usage(&self) -> Option<TokenUsage> {
        let total_input =
            self.input_tokens + self.cache_read_input_tokens + self.cache_creation_input_tokens;
        let usage = TokenUsage {
            input_tokens: total_input,
            cached_input_tokens: self.cache_read_input_tokens,
            output_tokens: self.output_tokens,
            reasoning_output_tokens: 0,
            total_tokens: total_input + self.output_tokens,
        };
        if usage.is_zero() { None } else { Some(usage) }
    }

    /// The opaque signature needed to replay the assistant thinking block on the
    /// next request. Extended thinking produces a single signed block per turn;
    /// if that is not the case we omit it and fall back to plain sampling.
    fn reasoning_signature(&self) -> Option<String> {
        let mut signatures = self
            .blocks
            .values()
            .filter(|block| block.kind == "thinking")
            .filter_map(|block| non_empty_string(block.signature.clone()));
        let signature = signatures.next()?;
        if signatures.next().is_some() {
            return None;
        }
        Some(signature)
    }

    fn into_stream_result(self) -> AgentTurnStreamResult {
        let mut items = Vec::new();
        let assistant_message = non_empty_string(self.assistant_text());
        if let Some(content) = assistant_message.clone() {
            items.push(AgentTurnItem::AssistantMessage { content });
        }
        let tool_calls = self.blocks.values().filter_map(|block| {
            if block.kind != "tool_use" {
                return None;
            }
            if block.tool_id.is_empty() || block.tool_name.is_empty() {
                return None;
            }
            let arguments = if block.partial_json.trim().is_empty() {
                if block.input_start.is_null() {
                    json!({})
                } else {
                    block.input_start.clone()
                }
            } else {
                serde_json::from_str(&block.partial_json).unwrap_or_else(|_| json!({}))
            };
            Some(AgentToolCall {
                id: block.tool_id.clone(),
                name: block.tool_name.clone(),
                arguments,
            })
        });
        let mut tool_call_count = 0usize;
        for call in tool_calls {
            tool_call_count += 1;
            items.push(AgentTurnItem::ToolCall { call });
        }
        AgentTurnStreamResult {
            items,
            raw_stream_follow_up: tool_call_count > 0,
            last_assistant_message: assistant_message,
            last_reasoning_content: non_empty_string(self.reasoning_text()),
            last_reasoning_signature: self.reasoning_signature(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ModelConfig, ThinkingBudget};

    fn test_client(model_id: &str) -> AnthropicCompatibleClient {
        AnthropicCompatibleClient::new(
            "test-key",
            "https://example.test/v1",
            &ModelConfig {
                model_id: model_id.to_string(),
                provider: "anthropic".to_string(),
                ..ModelConfig::default()
            },
        )
    }

    fn read_tool() -> AgentToolSpec {
        AgentToolSpec {
            name: "read".to_string(),
            description: "read a file".to_string(),
            input_spec: AgentToolInputSpec::JsonSchema {
                schema: json!({
                    "type": "object",
                    "properties": { "path": { "type": "string" } },
                    "required": ["path"],
                    "additionalProperties": false
                }),
            },
        }
    }

    #[test]
    fn messages_url_accepts_both_base_url_conventions() {
        assert_eq!(
            anthropic_messages_url("https://api.anthropic.com"),
            "https://api.anthropic.com/v1/messages"
        );
        assert_eq!(
            anthropic_messages_url("https://relay.example/v1"),
            "https://relay.example/v1/messages"
        );
        assert_eq!(
            anthropic_messages_url("https://relay.example/v1/"),
            "https://relay.example/v1/messages"
        );
        assert_eq!(
            anthropic_messages_url("https://relay.example/v1/messages"),
            "https://relay.example/v1/messages"
        );
    }

    #[test]
    fn agent_payload_hoists_system_and_maps_tool_turns() {
        let client = test_client("claude-sonnet-4-5");
        let payload = build_agent_payload(
            &client,
            AgentTurnRequest {
                messages: vec![
                    AgentMessage::system("base"),
                    AgentMessage::user("hi"),
                    AgentMessage::assistant_tool_call_protocol_with_reasoning(
                        None,
                        None,
                        vec![AgentToolCall {
                            id: "t1".to_string(),
                            name: "read".to_string(),
                            arguments: json!({ "path": "a" }),
                        }],
                    ),
                    AgentMessage::tool("t1", "read", "contents"),
                ],
                tools: vec![read_tool()],
            },
            false,
        );

        assert_eq!(payload["system"], "base");
        assert_eq!(payload["tool_choice"]["type"], "auto");
        let messages = payload["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[1]["role"], "assistant");
        assert_eq!(messages[1]["content"][0]["type"], "tool_use");
        assert_eq!(messages[1]["content"][0]["input"]["path"], "a");
        assert_eq!(messages[2]["role"], "user");
        assert_eq!(messages[2]["content"][0]["type"], "tool_result");
        assert_eq!(messages[2]["content"][0]["tool_use_id"], "t1");
    }

    #[test]
    fn agent_payload_merges_consecutive_user_messages() {
        let (_, messages) = agent_messages_to_anthropic(
            vec![
                AgentMessage::user("one"),
                AgentMessage::assistant("ack"),
                AgentMessage::user("two"),
                AgentMessage::user("three"),
            ],
            false,
            false,
        );
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[2]["content"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn orphan_tool_result_is_flattened_instead_of_rejected() {
        let (_, messages) = agent_messages_to_anthropic(
            vec![
                AgentMessage::user("hi"),
                AgentMessage::tool("missing", "read", "orphan"),
            ],
            false,
            false,
        );
        let last = messages.last().unwrap();
        assert_eq!(last["role"], "user");
        assert_eq!(last["content"][0]["type"], "text");
    }

    #[test]
    fn adaptive_thinking_survives_unsigned_tool_continuation() {
        let client = AnthropicCompatibleClient::new(
            "test-key",
            "https://example.test/v1",
            &ModelConfig {
                model_id: "claude-sonnet-4-5".to_string(),
                provider: "anthropic".to_string(),
                thinking_budget: Some(ThinkingBudget::new("high")),
                ..ModelConfig::default()
            },
        );
        let unsigned_continuation = || AgentTurnRequest {
            messages: vec![
                AgentMessage::user("hi"),
                AgentMessage::assistant_tool_call_protocol_with_reasoning(
                    None,
                    None,
                    vec![AgentToolCall {
                        id: "t1".to_string(),
                        name: "read".to_string(),
                        arguments: json!({}),
                    }],
                ),
                AgentMessage::tool("t1", "read", "contents"),
            ],
            tools: vec![read_tool()],
        };

        let fresh = build_agent_payload(
            &client,
            AgentTurnRequest {
                messages: vec![AgentMessage::user("hi")],
                tools: Vec::new(),
            },
            false,
        );
        assert_eq!(fresh["thinking"]["type"], "adaptive");
        assert_eq!(fresh["output_config"]["effort"], "high");
        assert!(fresh.get("temperature").is_none());

        // Adaptive thinking may skip a step; the next step must still ask for it.
        let continuation = build_agent_payload(&client, unsigned_continuation(), false);
        assert_eq!(continuation["thinking"]["type"], "adaptive");
        assert_eq!(continuation["output_config"]["effort"], "high");

        // The legacy budget shape still requires a signed thinking block.
        client
            .thinking_mode
            .store(THINKING_MODE_BUDGET, Ordering::Relaxed);
        let budget_continuation = build_agent_payload(&client, unsigned_continuation(), false);
        assert!(budget_continuation.get("thinking").is_none());
        assert!(budget_continuation.get("temperature").is_none());
    }

    #[test]
    fn thinking_continuation_replays_signed_block() {
        let client = AnthropicCompatibleClient::new(
            "test-key",
            "https://example.test/v1",
            &ModelConfig {
                model_id: "claude-sonnet-4-5".to_string(),
                provider: "anthropic".to_string(),
                thinking_budget: Some(ThinkingBudget::new("high")),
                ..ModelConfig::default()
            },
        );

        let payload = build_agent_payload(
            &client,
            AgentTurnRequest {
                messages: vec![
                    AgentMessage::user("hi"),
                    AgentMessage::assistant_tool_call_protocol_with_signed_reasoning(
                        None,
                        Some("planning".to_string()),
                        Some("sig-abc".to_string()),
                        vec![AgentToolCall {
                            id: "t1".to_string(),
                            name: "read".to_string(),
                            arguments: json!({}),
                        }],
                    ),
                    AgentMessage::tool("t1", "read", "contents"),
                ],
                tools: vec![read_tool()],
            },
            false,
        );

        let messages = payload["messages"].as_array().unwrap();
        let assistant_blocks = messages[1]["content"].as_array().unwrap();
        assert_eq!(assistant_blocks[0]["type"], "thinking");
        assert_eq!(assistant_blocks[0]["thinking"], "planning");
        assert_eq!(assistant_blocks[0]["signature"], "sig-abc");
        assert_eq!(assistant_blocks[1]["type"], "tool_use");
        assert_eq!(payload["thinking"]["type"], "adaptive");
        assert!(payload.get("temperature").is_none());
    }

    #[test]
    fn numeric_thinking_budget_maps_to_adaptive_effort() {
        let client = AnthropicCompatibleClient::new(
            "test-key",
            "https://example.test/v1",
            &ModelConfig {
                model_id: "claude-sonnet-4-5".to_string(),
                provider: "anthropic".to_string(),
                thinking_budget: Some(ThinkingBudget::new("4096")),
                context_window_tokens: 1_000_000,
                max_completion_tokens: 128_000,
                ..ModelConfig::default()
            },
        );
        let payload = build_agent_payload(
            &client,
            AgentTurnRequest {
                messages: vec![AgentMessage::user("hi")],
                tools: Vec::new(),
            },
            false,
        );
        assert_eq!(payload["thinking"]["type"], "adaptive");
        assert_eq!(payload["output_config"]["effort"], "medium");
        assert!(payload.get("temperature").is_none());
    }

    #[test]
    fn budget_mode_uses_explicit_token_shape() {
        let client = AnthropicCompatibleClient::new(
            "test-key",
            "https://example.test/v1",
            &ModelConfig {
                model_id: "claude-sonnet-4-5".to_string(),
                provider: "anthropic".to_string(),
                thinking_budget: Some(ThinkingBudget::new("4096")),
                context_window_tokens: 1_000_000,
                max_completion_tokens: 128_000,
                ..ModelConfig::default()
            },
        );
        // A provider that rejected adaptive thinking falls back to the older
        // explicit `enabled` + `budget_tokens` shape.
        client
            .thinking_mode
            .store(THINKING_MODE_BUDGET, Ordering::Relaxed);
        let payload = build_agent_payload(
            &client,
            AgentTurnRequest {
                messages: vec![AgentMessage::user("hi")],
                tools: Vec::new(),
            },
            false,
        );
        assert_eq!(payload["thinking"]["type"], "enabled");
        assert_eq!(payload["thinking"]["budget_tokens"], 4096);
        assert!(payload.get("output_config").is_none());
    }

    #[test]
    fn thinking_error_fallback_selects_the_other_shape_then_disables() {
        // Provider demands adaptive thinking: switch from AUTO/BUDGET to adaptive.
        assert_eq!(
            next_thinking_mode_on_error(
                r#"{"error":{"message":"claude-opus-5-5 requires adaptive thinking; omit thinking or use thinking.type=adaptive and output_config.effort"}}"#,
                THINKING_MODE_AUTO,
                true,
            ),
            Some(THINKING_MODE_ADAPTIVE)
        );
        // The provider requires adaptive thinking but the client cannot request
        // it: surface the error instead of silently disabling thinking.
        assert_eq!(
            next_thinking_mode_on_error("requires adaptive thinking", THINKING_MODE_AUTO, false),
            None
        );
        // Adaptive is required and already being sent: retrying cannot help.
        assert_eq!(
            next_thinking_mode_on_error("requires adaptive thinking", THINKING_MODE_ADAPTIVE, true),
            None
        );
        // Adaptive rejected: fall back to the explicit token-budget shape.
        assert_eq!(
            next_thinking_mode_on_error(
                r#"{"error":{"message":"thinking.type adaptive is not supported"}}"#,
                THINKING_MODE_ADAPTIVE,
                true,
            ),
            Some(THINKING_MODE_BUDGET)
        );
        // The budget shape was rejected as well: disable thinking.
        assert_eq!(
            next_thinking_mode_on_error("unknown parameter: thinking", THINKING_MODE_BUDGET, true),
            Some(THINKING_MODE_DISABLED)
        );
        // Unrelated errors leave thinking untouched.
        assert_eq!(
            next_thinking_mode_on_error("model not found", THINKING_MODE_AUTO, true),
            None
        );
    }

    #[test]
    fn adaptive_effort_maps_provider_agnostic_budgets() {
        assert_eq!(adaptive_effort("minimal"), Some("low"));
        assert_eq!(adaptive_effort("low"), Some("low"));
        assert_eq!(adaptive_effort("medium"), Some("medium"));
        assert_eq!(adaptive_effort("high"), Some("high"));
        assert_eq!(adaptive_effort("xhigh"), Some("xhigh"));
        assert_eq!(adaptive_effort("max"), Some("max"));
        assert_eq!(adaptive_effort("4096"), Some("medium"));
        assert_eq!(adaptive_effort("1024"), Some("low"));
        assert_eq!(adaptive_effort("200000"), Some("max"));
    }

    #[test]
    fn signed_thinking_block_is_replayed_without_visible_text() {
        let (_, messages) = agent_messages_to_anthropic(
            vec![
                AgentMessage::user("hi"),
                AgentMessage::assistant_tool_call_protocol_with_signed_reasoning(
                    None,
                    None,
                    Some("sig-only".to_string()),
                    vec![AgentToolCall {
                        id: "t1".to_string(),
                        name: "read".to_string(),
                        arguments: json!({}),
                    }],
                ),
                AgentMessage::tool("t1", "read", "contents"),
            ],
            false,
            true,
        );
        let blocks = messages[1]["content"].as_array().unwrap();
        assert_eq!(blocks[0]["type"], "thinking");
        assert_eq!(blocks[0]["thinking"], "");
        assert_eq!(blocks[0]["signature"], "sig-only");
    }

    #[test]
    fn stream_state_builds_text_and_tool_call_items() {
        let mut state = AnthropicStreamState::default();
        state
            .apply_event(&json!({
                "type": "message_start",
                "message": { "usage": { "input_tokens": 10, "cache_read_input_tokens": 5 } }
            }))
            .unwrap();
        state
            .apply_event(&json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": { "type": "text", "text": "" }
            }))
            .unwrap();
        state
            .apply_event(&json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": { "type": "text_delta", "text": "hello" }
            }))
            .unwrap();
        state
            .apply_event(&json!({
                "type": "content_block_start",
                "index": 1,
                "content_block": { "type": "tool_use", "id": "toolu_1", "name": "read", "input": {} }
            }))
            .unwrap();
        state
            .apply_event(&json!({
                "type": "content_block_delta",
                "index": 1,
                "delta": { "type": "input_json_delta", "partial_json": "{\"path\":" }
            }))
            .unwrap();
        state
            .apply_event(&json!({
                "type": "content_block_delta",
                "index": 1,
                "delta": { "type": "input_json_delta", "partial_json": "\"a\"}" }
            }))
            .unwrap();
        state
            .apply_event(&json!({
                "type": "content_block_start",
                "index": 2,
                "content_block": { "type": "thinking", "thinking": "" }
            }))
            .unwrap();
        state
            .apply_event(&json!({
                "type": "content_block_delta",
                "index": 2,
                "delta": { "type": "thinking_delta", "thinking": "hmm" }
            }))
            .unwrap();
        state
            .apply_event(&json!({
                "type": "content_block_delta",
                "index": 2,
                "delta": { "type": "signature_delta", "signature": "sig-abc" }
            }))
            .unwrap();
        state
            .apply_event(&json!({
                "type": "message_delta",
                "delta": { "stop_reason": "tool_use" },
                "usage": { "output_tokens": 7 }
            }))
            .unwrap();
        state
            .apply_event(&json!({ "type": "message_stop" }))
            .unwrap();

        let usage = state.token_usage().unwrap();
        assert_eq!(usage.input_tokens, 15);
        assert_eq!(usage.cached_input_tokens, 5);
        assert_eq!(usage.output_tokens, 7);

        let result = state.into_stream_result();
        assert!(result.raw_stream_follow_up);
        assert_eq!(result.last_assistant_message.as_deref(), Some("hello"));
        let call = result
            .items
            .iter()
            .find_map(|item| match item {
                AgentTurnItem::ToolCall { call } => Some(call.clone()),
                _ => None,
            })
            .unwrap();
        assert_eq!(call.name, "read");
        assert_eq!(call.arguments["path"], "a");
        assert_eq!(result.last_reasoning_content.as_deref(), Some("hmm"));
        assert_eq!(result.last_reasoning_signature.as_deref(), Some("sig-abc"));
    }
}
