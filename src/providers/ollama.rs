use std::{
    collections::{HashSet, VecDeque},
    sync::{Arc, LazyLock, Mutex},
    time::{Duration, Instant},
};

use async_trait::async_trait;
use futures_util::StreamExt;
use miette::{Result, miette};
use parking_lot::Mutex as ParkingLotMutex;
use serde_json::{Value, json};
use tracing::warn;

use crate::{
    config::ModelConfig,
    context::Context,
    context_budget::{
        ContextBudgetExceededError, RequestBudgetLimits, estimate_agent_turn_request,
        estimate_prompt_request,
    },
    core::{Llm, TokenUsage, TokenUsageInfo},
    dsml_repair,
    model_catalog::catalog_model_capacity,
    providers::io::{
        default_rate_limit_backoff, format_request_error, looks_like_context_window_error,
        looks_like_vision_unsupported_error, non_empty_string, parse_retry_after_seconds,
        should_retry_request_without_thinking_budget, summarize_agent_turn_request,
        summarize_prompt_request, truncate_for_error,
    },
    providers::thinking::thinking_budget_is_none,
    reasoning::runtime::{
        AgentContent, AgentContentPart, AgentMessage, AgentToolCall, AgentToolInputSpec,
        AgentTurnItem, AgentTurnRequest, AgentTurnStreamResult, PromptRequest,
    },
};

const DEFAULT_OLLAMA_HOST: &str = "http://127.0.0.1:11434";

type RequestRateLimiter = Arc<tokio::sync::Mutex<VecDeque<Instant>>>;
type RequestRateLimiterMap = std::collections::HashMap<String, RequestRateLimiter>;

static REQUEST_RATE_LIMITERS: LazyLock<ParkingLotMutex<RequestRateLimiterMap>> =
    LazyLock::new(|| ParkingLotMutex::new(std::collections::HashMap::new()));

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OllamaThinkingMode {
    Enabled,
    Unsupported,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OllamaVisionMode {
    Enabled,
    Disabled,
}

fn parse_host(host: Option<&str>) -> String {
    let host = host.unwrap_or("").trim();
    if host.is_empty() {
        return DEFAULT_OLLAMA_HOST.to_string();
    }
    if host.starts_with("http://") || host.starts_with("https://") {
        host.to_string()
    } else {
        format!("http://{host}")
    }
}

fn ollama_prompt_response_to_value(
    response_json: &Value,
) -> Result<(serde_json::Value, Option<TokenUsage>)> {
    let message = &response_json["message"];
    let content = message["content"].as_str().unwrap_or("");
    let tool_calls = message.get("tool_calls").and_then(Value::as_array);

    let usage = parse_usage_from_ollama_response(response_json);

    match tool_calls {
        Some(tool_calls) if !tool_calls.is_empty() => {
            let first = &tool_calls[0];
            let function = &first["function"];
            let arguments = if let Some(args_str) = function["arguments"].as_str() {
                serde_json::from_str(args_str).map_err(|err| {
                    miette!("failed to decode tool arguments as JSON: {err}; arguments={args_str}")
                })?
            } else {
                function["arguments"].clone()
            };
            Ok((arguments, usage))
        }
        _ => {
            if let Some(value) = extract_json_value_from_content(content) {
                return Ok((value, usage));
            }
            let cleaned = content.trim();
            Ok((json!(cleaned), usage))
        }
    }
}

fn extract_json_value_from_content(content: &str) -> Option<Value> {
    let content = content.trim();
    if let Some(fenced) = content
        .strip_prefix("```json")
        .and_then(|s| s.strip_suffix("```"))
        .or(content
            .strip_prefix("```")
            .and_then(|s| s.strip_suffix("```")))
        && let Ok(value) = serde_json::from_str::<Value>(fenced)
    {
        return Some(value);
    }
    if (content.starts_with('{') || content.starts_with('['))
        && let Ok(value) = serde_json::from_str::<Value>(content)
    {
        return Some(value);
    }
    None
}

pub struct OllamaClient {
    client: reqwest::Client,
    host: String,
    api_key: Option<String>,
    model: String,
    temperature: f64,
    thinking_budget: Option<String>,
    thinking_mode: Mutex<OllamaThinkingMode>,
    vision_mode: Mutex<OllamaVisionMode>,
    rpm: Option<usize>,
    request_rate_limiter: Option<RequestRateLimiter>,
    keep_alive: Option<String>,
    stream_idle_timeout: Duration,
    effective_context_window_tokens: usize,
    auto_compact_threshold_tokens: usize,
    reserved_output_tokens: usize,
    max_completion_tokens: usize,
    token_usage: Mutex<TokenUsageInfo>,
}

impl OllamaClient {
    pub fn from_parts(
        host: Option<&str>,
        model_config: &ModelConfig,
        api_key: Option<&str>,
        keep_alive: Option<&str>,
    ) -> Self {
        let host = parse_host(host);
        let request_timeout = Duration::from_secs(model_config.request_timeout_secs());
        let stream_idle_timeout = Duration::from_secs(model_config.stream_idle_timeout_secs());
        let client = reqwest::Client::builder()
            .timeout(request_timeout)
            .build()
            .expect("failed to build ollama http client");
        let context_window_tokens = model_config.context_window_tokens();
        let effective_context_window_tokens = model_config.effective_context_window_tokens();
        let auto_compact_threshold_tokens = model_config.auto_compact_token_limit();
        let reserved_output_tokens = model_config.reserved_output_tokens();
        let max_completion_tokens = model_config.max_completion_tokens();
        let vision_mode_initial = match model_config.supports_vision {
            Some(false) => OllamaVisionMode::Disabled,
            _ => {
                let catalog = catalog_model_capacity(&model_config.model_id);
                let supports = catalog.map(|c| c.supports_vision).unwrap_or(false);
                if supports {
                    OllamaVisionMode::Enabled
                } else {
                    OllamaVisionMode::Disabled
                }
            }
        };
        let thinking_mode_initial = if model_config.thinking_budget().is_some() {
            OllamaThinkingMode::Enabled
        } else {
            OllamaThinkingMode::Unsupported
        };
        Self {
            client,
            host: host.clone(),
            api_key: api_key.map(|s| s.to_string()).filter(|s| !s.is_empty()),
            model: model_config.model_id.clone(),
            temperature: model_config.temperature,
            thinking_budget: model_config
                .thinking_budget()
                .map(|budget| budget.as_str().to_string()),
            thinking_mode: Mutex::new(thinking_mode_initial),
            vision_mode: Mutex::new(vision_mode_initial),
            rpm: model_config.rpm(),
            request_rate_limiter: shared_ollama_rate_limiter(
                &host,
                &model_config.model_id,
                model_config.rpm(),
            ),
            keep_alive: keep_alive.map(|s| s.to_string()).filter(|s| !s.is_empty()),
            stream_idle_timeout,
            effective_context_window_tokens,
            auto_compact_threshold_tokens,
            reserved_output_tokens,
            max_completion_tokens,
            token_usage: Mutex::new(TokenUsageInfo {
                total_token_usage: TokenUsage::default(),
                last_token_usage: TokenUsage::default(),
                model_context_window: Some(context_window_tokens as i64),
                daily_token_usage: Vec::new(),
            }),
        }
    }

    fn chat_url(&self) -> String {
        format!("{}/api/chat", self.host)
    }

    fn request_budget_limits(&self) -> RequestBudgetLimits {
        RequestBudgetLimits {
            context_window_tokens: self.effective_context_window_tokens,
            auto_compact_threshold_tokens: self.auto_compact_threshold_tokens,
            reserved_output_tokens: self.reserved_output_tokens,
        }
    }

    fn auth_header(&self) -> Option<String> {
        self.api_key.as_deref().map(|key| {
            if key.starts_with("Bearer ") {
                key.to_string()
            } else {
                format!("Bearer {key}")
            }
        })
    }

    fn record_usage(&self, usage: Option<TokenUsage>) {
        if let Some(usage) = usage
            && let Ok(mut info) = self.token_usage.lock()
        {
            info.append_last_usage(usage);
        }
    }

    fn should_inject_think(&self) -> bool {
        let Ok(mode) = self.thinking_mode.lock() else {
            return false;
        };
        *mode == OllamaThinkingMode::Enabled && self.thinking_budget.is_some()
    }

    fn mark_thinking_unsupported(&self) {
        if let Ok(mut mode) = self.thinking_mode.lock() {
            *mode = OllamaThinkingMode::Unsupported;
        }
    }

    fn vision_disabled(&self) -> bool {
        let Ok(mode) = self.vision_mode.lock() else {
            return false;
        };
        *mode == OllamaVisionMode::Disabled
    }

    fn mark_vision_disabled(&self) {
        if let Ok(mut mode) = self.vision_mode.lock() {
            *mode = OllamaVisionMode::Disabled;
        }
    }

    async fn wait_for_request_slot(&self, request_context: &[String]) {
        let Some(rpm) = self.rpm else { return };
        let Some(limiter) = &self.request_rate_limiter else {
            return;
        };
        let mut logged_wait = false;
        loop {
            let delay = {
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
            let Some(delay) = delay else { return };
            if !logged_wait {
                warn!(
                    "ollama rpm throttle waiting {} ms before next request (rpm={})\n{}",
                    delay.as_millis(),
                    rpm,
                    request_context.join("\n")
                );
                logged_wait = true;
            }
            tokio::time::sleep(delay).await;
        }
    }

    async fn post_ollama_chat(
        &self,
        payload: &Value,
        request_context: &[String],
    ) -> Result<(Value, Option<TokenUsage>)> {
        const MAX_429_RETRIES: usize = 4;
        const MAX_5XX_RETRIES: usize = 4;

        let url = self.chat_url();
        let mut rate_limit_attempt = 0usize;
        let mut transient_attempt = 0usize;
        loop {
            self.wait_for_request_slot(request_context).await;
            let mut req = self.client.post(&url).json(payload);
            if let Some(auth) = self.auth_header() {
                req = req.header("Authorization", &auth);
            }
            let response = req.send().await.map_err(|err| {
                format_request_error("ollama chat request failed", &url, request_context, &err)
            })?;

            let status = response.status();

            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                let retry_after = response
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|value| value.to_str().ok())
                    .and_then(parse_retry_after_seconds);
                let body = response.text().await.unwrap_or_default();
                if rate_limit_attempt >= MAX_429_RETRIES {
                    return Err(miette!(
                        "ollama api returned HTTP 429 after {MAX_429_RETRIES} retries: {}",
                        truncate_for_error(&body)
                    ));
                }
                let delay = retry_after
                    .map(Duration::from_secs)
                    .unwrap_or_else(|| default_rate_limit_backoff(rate_limit_attempt));
                warn!(
                    "ollama api returned HTTP 429; retrying in {} ms (attempt {}/{})\n{}",
                    delay.as_millis(),
                    rate_limit_attempt + 1,
                    MAX_429_RETRIES,
                    request_context.join("\n")
                );
                tokio::time::sleep(delay).await;
                rate_limit_attempt += 1;
                continue;
            }

            if status.is_server_error() {
                let body = response.text().await.unwrap_or_default();
                if transient_attempt >= MAX_5XX_RETRIES {
                    return Err(miette!(
                        "ollama api returned HTTP {status} after {MAX_5XX_RETRIES} retries: {}",
                        truncate_for_error(&body)
                    ));
                }
                let delay = Duration::from_millis(400 * (1u64 << transient_attempt));
                warn!(
                    "ollama api returned HTTP {status}; retrying in {} ms (attempt {}/{})\n{}",
                    delay.as_millis(),
                    transient_attempt + 1,
                    MAX_5XX_RETRIES,
                    request_context.join("\n")
                );
                tokio::time::sleep(delay).await;
                transient_attempt += 1;
                continue;
            }

            let body = response
                .text()
                .await
                .map_err(|err| miette!("ollama response body read failed: {err}"))?;

            if !status.is_success() {
                return Err(miette!(
                    "ollama api returned HTTP {status}: {}",
                    truncate_for_error(&body)
                ));
            }

            let response_json: Value = serde_json::from_str(&body).map_err(|err| {
                miette!(
                    "ollama response is not valid JSON: {err}; body={}",
                    truncate_for_error(&body)
                )
            })?;
            if let Some(error) = response_json.get("error").and_then(Value::as_str) {
                return Err(miette!("ollama api error: {error}"));
            }
            let usage = parse_usage_from_ollama_response(&response_json);
            return Ok((response_json, usage));
        }
    }

    async fn post_ollama_chat_with_adaptive_retry(
        &self,
        payload: &Value,
        request_context: &[String],
        request_kind: &str,
        budget: &crate::context_budget::RequestBudgetBreakdown,
        max_retries: usize,
    ) -> Result<(Value, Option<TokenUsage>)> {
        let mut attempt = 0usize;
        let mut current_payload = payload.clone();
        loop {
            match self
                .post_ollama_chat(&current_payload, request_context)
                .await
            {
                Ok(result) => return Ok(result),
                Err(err) => {
                    let err_str = err.to_string();
                    if looks_like_context_window_error(&err_str) {
                        return Err(ContextBudgetExceededError::for_request(
                            request_kind,
                            &self.model,
                            budget,
                            Some(&format!("provider_body={}", truncate_for_error(&err_str))),
                        )
                        .into());
                    }
                    if attempt >= max_retries {
                        return Err(err);
                    }
                    let should_retry_thinking = self.should_inject_think()
                        && (should_retry_request_without_thinking_budget(&err_str)
                            || err_str.contains("think parameter rejected"));
                    if should_retry_thinking {
                        self.mark_thinking_unsupported();
                        if let Some(obj) = current_payload.as_object_mut() {
                            obj.remove("think");
                        }
                        attempt += 1;
                        warn!(
                            "ollama provider rejected think; retrying {} without it (attempt {}/{})\n{}",
                            request_kind,
                            attempt,
                            max_retries,
                            request_context.join("\n")
                        );
                        continue;
                    }
                    if !self.vision_disabled() && looks_like_vision_unsupported_error(&err_str) {
                        self.mark_vision_disabled();
                        warn!(
                            "ollama provider rejected image input; disabling vision for {} (no retry in non-streaming path)\n{}",
                            request_kind,
                            request_context.join("\n")
                        );
                    }
                    return Err(err);
                }
            }
        }
    }

    async fn send_ollama_stream(
        &self,
        payload: &Value,
        request_context: &[String],
    ) -> Result<reqwest::Response> {
        self.wait_for_request_slot(request_context).await;
        let url = self.chat_url();
        let mut req = self.client.post(&url).json(payload);
        if let Some(auth) = self.auth_header() {
            req = req.header("Authorization", &auth);
        }
        req.send().await.map_err(|err| {
            format_request_error(
                "ollama chat stream request failed",
                &url,
                request_context,
                &err,
            )
        })
    }

    async fn call_ollama_stream(
        &self,
        context: &Context,
        payload: &Value,
        budget: &crate::context_budget::RequestBudgetBreakdown,
        request: &AgentTurnRequest,
    ) -> Result<AgentTurnStreamResult> {
        let request_context = summarize_agent_turn_request(request, Some(budget));
        let allowed_tool_names: HashSet<String> =
            request.tools.iter().map(|t| t.name.clone()).collect();

        let mut current_payload = payload.clone();
        let mut adaptive_attempt = 0usize;
        let mut rate_limit_attempt = 0usize;
        let mut transient_attempt = 0usize;

        const MAX_ADAPTIVE_RETRIES: usize = 2;
        const MAX_429_RETRIES: usize = 4;
        const MAX_5XX_RETRIES: usize = 4;

        loop {
            let response = self
                .send_ollama_stream(&current_payload, &request_context)
                .await?;
            let status = response.status();

            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                let retry_after = response
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|value| value.to_str().ok())
                    .and_then(parse_retry_after_seconds);
                let body = response.text().await.unwrap_or_default();
                if rate_limit_attempt >= MAX_429_RETRIES {
                    return Err(miette!(
                        "ollama stream: api returned HTTP 429 after {MAX_429_RETRIES} retries: {}",
                        truncate_for_error(&body)
                    ));
                }
                let delay = retry_after
                    .map(Duration::from_secs)
                    .unwrap_or_else(|| default_rate_limit_backoff(rate_limit_attempt));
                warn!(
                    "ollama stream: api returned HTTP 429; retrying in {} ms (attempt {}/{})\n{}",
                    delay.as_millis(),
                    rate_limit_attempt + 1,
                    MAX_429_RETRIES,
                    request_context.join("\n")
                );
                tokio::time::sleep(delay).await;
                rate_limit_attempt += 1;
                continue;
            }

            if status.is_server_error() {
                let body = response.text().await.unwrap_or_default();
                if transient_attempt >= MAX_5XX_RETRIES {
                    return Err(miette!(
                        "ollama stream: api returned HTTP {status} after {MAX_5XX_RETRIES} retries: {}",
                        truncate_for_error(&body)
                    ));
                }
                let delay = Duration::from_millis(400 * (1u64 << transient_attempt));
                warn!(
                    "ollama stream: api returned HTTP {status}; retrying in {} ms (attempt {}/{})\n{}",
                    delay.as_millis(),
                    transient_attempt + 1,
                    MAX_5XX_RETRIES,
                    request_context.join("\n")
                );
                tokio::time::sleep(delay).await;
                transient_attempt += 1;
                continue;
            }

            if !status.is_success() {
                let body = response.text().await.unwrap_or_default();

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

                if adaptive_attempt >= MAX_ADAPTIVE_RETRIES {
                    return Err(miette!(
                        "ollama api returned HTTP {status}: {}",
                        truncate_for_error(&body)
                    ));
                }

                if self.should_inject_think()
                    && (should_retry_request_without_thinking_budget(&body)
                        || body.to_ascii_lowercase().contains("think"))
                {
                    self.mark_thinking_unsupported();
                    if let Some(obj) = current_payload.as_object_mut() {
                        obj.remove("think");
                    }
                    adaptive_attempt += 1;
                    warn!(
                        "ollama stream: provider rejected think parameter; retrying without it (attempt {}/{})\n{}",
                        adaptive_attempt,
                        MAX_ADAPTIVE_RETRIES,
                        request_context.join("\n")
                    );
                    continue;
                }

                if !self.vision_disabled() && looks_like_vision_unsupported_error(&body) {
                    self.mark_vision_disabled();
                    adaptive_attempt += 1;
                    let messages = agent_turn_request_to_ollama_messages_stripped(request);
                    current_payload["messages"] = json!(messages);
                    warn!(
                        "ollama stream: provider rejected image input; retrying without images (attempt {}/{})\n{}",
                        adaptive_attempt,
                        MAX_ADAPTIVE_RETRIES,
                        request_context.join("\n")
                    );
                    continue;
                }

                return Err(miette!(
                    "ollama api returned HTTP {status}: {}",
                    truncate_for_error(&body)
                ));
            }

            return self
                .parse_ollama_stream(context, response, &allowed_tool_names)
                .await;
        }
    }

    async fn parse_ollama_stream(
        &self,
        context: &Context,
        response: reqwest::Response,
        allowed_tool_names: &HashSet<String>,
    ) -> Result<AgentTurnStreamResult> {
        let mut content = String::new();
        let mut thinking = String::new();
        let mut tool_calls: Vec<AgentToolCall> = Vec::new();
        let mut last_usage = None;
        let mut last_assistant_progress_emit_at = Instant::now();
        let mut last_assistant_progress_char_len = 0usize;
        let mut last_reasoning_progress_emit_at = Instant::now();
        let mut last_reasoning_progress_char_len = 0usize;
        let mut buffer = String::new();

        let bytes_stream = response.bytes_stream();
        futures_util::pin_mut!(bytes_stream);

        loop {
            let next_chunk = tokio::time::timeout(self.stream_idle_timeout, bytes_stream.next())
                .await
                .map_err(|_| {
                    miette!(
                        "ollama streaming response stalled for over {}s (model={}, url={})",
                        self.stream_idle_timeout.as_secs(),
                        self.model,
                        self.chat_url()
                    )
                })?;
            let Some(chunk) = next_chunk else {
                break;
            };
            let chunk =
                chunk.map_err(|err| miette!("ollama streaming chunk read failed: {err}"))?;
            buffer.push_str(&String::from_utf8_lossy(&chunk));

            while let Some(line_end) = buffer.find('\n') {
                let line = buffer[..line_end].trim().to_string();
                buffer.drain(..=line_end);
                if line.is_empty() {
                    continue;
                }
                let part: Value = match serde_json::from_str(&line) {
                    Ok(v) => v,
                    Err(err) => {
                        warn!(
                            "ollama stream line not valid JSON: {err}; line={}",
                            truncate_for_error(&line)
                        );
                        continue;
                    }
                };
                if let Some(error) = part.get("error").and_then(Value::as_str) {
                    return Err(miette!("ollama stream api error: {error}"));
                }
                if let Some(usage) = parse_usage_from_ollama_response(&part) {
                    last_usage = Some(usage);
                }

                let message = &part["message"];

                if let Some(delta_content) = message["content"].as_str() {
                    content.push_str(delta_content);
                    let should_emit = content
                        .chars()
                        .count()
                        .saturating_sub(last_assistant_progress_char_len)
                        >= 64
                        || last_assistant_progress_emit_at.elapsed() >= Duration::from_millis(800);
                    if should_emit && !content.trim().is_empty() {
                        context.emit_live_assistant_progress(&content);
                        last_assistant_progress_emit_at = Instant::now();
                        last_assistant_progress_char_len = content.chars().count();
                    }
                }

                if let Some(delta_thinking) = message["thinking"].as_str() {
                    thinking.push_str(delta_thinking);
                    let should_emit = thinking
                        .chars()
                        .count()
                        .saturating_sub(last_reasoning_progress_char_len)
                        >= 64
                        || last_reasoning_progress_emit_at.elapsed() >= Duration::from_millis(800);
                    if should_emit && !thinking.trim().is_empty() {
                        context.emit_live_reasoning_progress(&thinking);
                        last_reasoning_progress_emit_at = Instant::now();
                        last_reasoning_progress_char_len = thinking.chars().count();
                    }
                }

                if let Some(tc_array) = message["tool_calls"].as_array()
                    && !tc_array.is_empty()
                {
                    let mut parsed = Vec::with_capacity(tc_array.len());
                    for (i, tc) in tc_array.iter().enumerate() {
                        let function = &tc["function"];
                        let name = function["name"].as_str().unwrap_or("");
                        let arguments = if let Some(args_str) = function["arguments"].as_str() {
                            serde_json::from_str(args_str).unwrap_or_else(|_| json!({}))
                        } else {
                            function["arguments"].clone()
                        };
                        let id = tc["id"]
                            .as_str()
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| format!("call_{i}"));
                        if !name.is_empty() {
                            parsed.push(AgentToolCall {
                                id,
                                name: name.to_string(),
                                arguments,
                            });
                        }
                    }
                    if !parsed.is_empty() {
                        tool_calls = parsed;
                    }
                }

                if part.get("done").and_then(Value::as_bool).unwrap_or(false) {
                    break;
                }
            }
        }

        if !thinking.trim().is_empty()
            && thinking.chars().count() != last_reasoning_progress_char_len
        {
            context.emit_live_reasoning_progress(&thinking);
        }
        if !content.trim().is_empty() && content.chars().count() != last_assistant_progress_char_len
        {
            context.emit_live_assistant_progress(&content);
        }
        if let Some(usage) = last_usage {
            self.record_usage(Some(usage));
        }

        if tool_calls.is_empty() && !allowed_tool_names.is_empty() {
            let combined = format!("{thinking}\n{content}");
            let scavenged = dsml_repair::scavenge_dsml_tool_calls(&combined, allowed_tool_names, 4);
            if !scavenged.is_empty() {
                tool_calls = scavenged;
            }
        }

        for call in &mut tool_calls {
            dsml_repair::repair_tool_call_arguments(call);
        }

        let cleaned_thinking = dsml_repair::strip_dsml_from_thinking(&thinking);
        let cleaned_content = dsml_repair::strip_dsml_from_thinking(&content);

        if !tool_calls.is_empty() {
            let assistant_message = if cleaned_content.trim().is_empty() {
                None
            } else {
                Some(cleaned_content)
            };
            let mut items =
                Vec::with_capacity(tool_calls.len() + usize::from(assistant_message.is_some()));
            if let Some(content) = assistant_message.clone() {
                items.push(AgentTurnItem::AssistantMessage { content });
            }
            items.extend(
                tool_calls
                    .into_iter()
                    .map(|call| AgentTurnItem::ToolCall { call }),
            );
            return Ok(AgentTurnStreamResult {
                items,
                raw_stream_follow_up: true,
                last_assistant_message: assistant_message,
                last_reasoning_content: non_empty_string(cleaned_thinking),
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
            last_reasoning_content: non_empty_string(cleaned_thinking),
        })
    }
}

#[async_trait]
impl Llm for OllamaClient {
    async fn run_json(&self, _context: &Context, request: PromptRequest) -> Result<Value> {
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
        let output_schema = request.output_schema.clone();
        let mut payload = json!({
            "model": self.model,
            "messages": prompt_request_to_ollama_messages(&request),
            "stream": false,
        });

        if !output_schema.is_null() {
            payload["format"] = output_schema;
        }

        if let Some(budget) = self.thinking_budget.as_deref()
            && self.should_inject_think()
        {
            inject_ollama_think(&mut payload, budget);
        }
        if let Some(keep_alive) = &self.keep_alive {
            payload["keep_alive"] = json!(keep_alive);
        }
        payload["options"] = build_ollama_options(
            self.temperature,
            self.effective_context_window_tokens,
            self.max_completion_tokens,
        );

        let (response, usage) = self
            .post_ollama_chat_with_adaptive_retry(
                &payload,
                &request_context,
                "prompt request",
                &budget,
                2,
            )
            .await?;
        self.record_usage(usage);
        let (value, _) = ollama_prompt_response_to_value(&response)?;
        Ok(value)
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

        let strip_images = self.vision_disabled();
        let messages = if strip_images {
            agent_turn_request_to_ollama_messages_stripped(&request)
        } else {
            agent_turn_request_to_ollama_messages(&request)
        };
        let tools = request
            .tools
            .iter()
            .map(|tool| {
                let (description, parameters) = match &tool.input_spec {
                    AgentToolInputSpec::JsonSchema { schema } => (
                        tool.description.clone(),
                        schema.clone(),
                    ),
                    AgentToolInputSpec::FreeformGrammar {
                        syntax,
                        definition,
                        fallback_schema,
                    } => (
                        format!(
                            "{}\n\nThis is a FREEFORM grammar tool. Put the complete tool input in the `input` field.\nsyntax={syntax}\ndefinition=\n{definition}",
                            tool.description
                        ),
                        fallback_schema.clone(),
                    ),
                };
                json!({
                    "type": "function",
                    "function": {
                        "name": tool.name,
                        "description": description,
                        "parameters": parameters,
                    }
                })
            })
            .collect::<Vec<_>>();

        let mut payload = json!({
            "model": self.model,
            "messages": messages,
            "stream": true,
        });
        if let Some(budget) = self.thinking_budget.as_deref()
            && self.should_inject_think()
        {
            inject_ollama_think(&mut payload, budget);
        }
        if !tools.is_empty() {
            payload["tools"] = json!(tools);
        }
        if let Some(keep_alive) = &self.keep_alive {
            payload["keep_alive"] = json!(keep_alive);
        }
        payload["options"] = build_ollama_options(
            self.temperature,
            self.effective_context_window_tokens,
            self.max_completion_tokens,
        );

        self.call_ollama_stream(context, &payload, &budget, &request)
            .await
    }

    fn token_usage_info(&self) -> Option<TokenUsageInfo> {
        self.token_usage.lock().ok().map(|info| info.clone())
    }

    fn model_name(&self) -> Option<String> {
        Some(self.model.clone())
    }
}

fn build_ollama_options(temperature: f64, num_ctx: usize, num_predict: usize) -> Value {
    json!({
        "temperature": temperature,
        "num_ctx": num_ctx,
        "num_predict": num_predict,
    })
}

fn inject_ollama_think(payload: &mut Value, budget: &str) {
    if thinking_budget_is_none(budget) {
        return;
    }
    payload["think"] = json!(budget);
}

fn agent_message_to_ollama_content(
    message: &AgentMessage,
    flatten_orphan_tool_results: bool,
    valid_tool_call_ids: &HashSet<String>,
    strip_images: bool,
) -> Option<Value> {
    match message {
        AgentMessage::System { content } => Some(json!({
            "role": "system",
            "content": content,
        })),
        AgentMessage::User { content } => {
            let (text, images) = extract_ollama_multimodal_content(content);
            let mut msg = json!({
                "role": "user",
                "content": text,
            });
            if !strip_images && !images.is_empty() {
                msg["images"] = json!(images);
            }
            Some(msg)
        }
        AgentMessage::Assistant { content } => Some(json!({
            "role": "assistant",
            "content": content,
        })),
        AgentMessage::AssistantToolCallProtocol {
            content,
            reasoning_content,
            calls,
        } => {
            let text = content.as_deref().unwrap_or("");
            let calls_json = calls
                .iter()
                .map(|call| {
                    json!({
                        "function": {
                            "name": call.name,
                            "arguments": call.arguments,
                        }
                    })
                })
                .collect::<Vec<_>>();
            let mut msg = json!({
                "role": "assistant",
                "content": text,
            });
            if !calls_json.is_empty() {
                msg["tool_calls"] = json!(calls_json);
            }
            if let Some(thinking) = reasoning_content
                && !thinking.trim().is_empty()
            {
                msg["thinking"] = json!(thinking);
            }
            Some(msg)
        }
        AgentMessage::Tool {
            tool_call_id,
            name,
            content,
        } => {
            if flatten_orphan_tool_results && !valid_tool_call_ids.contains(tool_call_id) {
                Some(json!({
                    "role": "assistant",
                    "content": format!("historical tool result ({name}):\n{content}"),
                }))
            } else {
                Some(json!({
                    "role": "tool",
                    "content": content,
                }))
            }
        }
    }
}

fn extract_ollama_multimodal_content(content: &AgentContent) -> (String, Vec<String>) {
    let text = content.as_text().to_string();
    let mut images = Vec::new();
    for part in content.parts() {
        match part {
            AgentContentPart::Image {
                path, media_type, ..
            } => {
                let data_url = image_part_data_url_ollama(path, media_type);
                if let Some(url) = data_url {
                    images.push(url);
                }
            }
            AgentContentPart::Text { .. } => {}
        }
    }
    (text, images)
}

fn image_part_data_url_ollama(path: &str, media_type: &str) -> Option<String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) => {
            tracing::warn!("failed to read multimodal image attachment {path}: {err}");
            return None;
        }
    };
    let media_type = normalize_ollama_image_media_type(path, media_type)?;
    Some(format!(
        "data:{media_type};base64,{}",
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes)
    ))
}

fn normalize_ollama_image_media_type(path: &str, media_type: &str) -> Option<String> {
    let media_type = media_type.trim();
    if media_type.starts_with("image/") {
        return Some(media_type.to_string());
    }
    match std::path::Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("png") => Some("image/png".to_string()),
        Some("jpg") | Some("jpeg") => Some("image/jpeg".to_string()),
        Some("webp") => Some("image/webp".to_string()),
        Some("gif") => Some("image/gif".to_string()),
        _ => {
            tracing::warn!(
                "failed to infer image MIME type for multimodal attachment {path}: media_type={media_type}"
            );
            None
        }
    }
}

fn prompt_request_to_ollama_messages(request: &PromptRequest) -> Vec<Value> {
    let mut messages = Vec::new();
    for msg in &request.system_messages {
        messages.push(json!({"role": "system", "content": msg}));
    }
    for msg in request.all_messages() {
        if let Some(m) = agent_message_to_ollama_content(&msg.message, true, &HashSet::new(), false)
        {
            messages.push(m);
        }
    }
    messages
}

fn agent_turn_request_to_ollama_messages(request: &AgentTurnRequest) -> Vec<Value> {
    agent_turn_request_to_ollama_messages_inner(request, false)
}

fn agent_turn_request_to_ollama_messages_stripped(request: &AgentTurnRequest) -> Vec<Value> {
    agent_turn_request_to_ollama_messages_inner(request, true)
}

fn agent_turn_request_to_ollama_messages_inner(
    request: &AgentTurnRequest,
    strip_images: bool,
) -> Vec<Value> {
    let mut valid_tool_call_ids = HashSet::new();
    for msg in &request.messages {
        if let AgentMessage::AssistantToolCallProtocol { calls, .. } = msg {
            for call in calls {
                valid_tool_call_ids.insert(call.id.clone());
            }
        }
    }

    let flatten = true;
    let mut messages = Vec::new();
    for msg in &request.messages {
        if let Some(m) =
            agent_message_to_ollama_content(msg, flatten, &valid_tool_call_ids, strip_images)
        {
            messages.push(m);
        }
    }
    messages
}

fn parse_usage_from_ollama_response(response: &Value) -> Option<TokenUsage> {
    let prompt_eval_count = response
        .get("prompt_eval_count")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let eval_count = response
        .get("eval_count")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let total = prompt_eval_count + eval_count;
    if total == 0 {
        return None;
    }
    Some(TokenUsage {
        input_tokens: prompt_eval_count,
        cached_input_tokens: 0,
        output_tokens: eval_count,
        reasoning_output_tokens: 0,
        total_tokens: total,
    })
}

fn shared_ollama_rate_limiter(
    host: &str,
    model_id: &str,
    rpm: Option<usize>,
) -> Option<RequestRateLimiter> {
    let rpm = rpm?;
    let key = format!(
        "{}\u{1f}{}\u{1f}{}",
        host.trim_end_matches('/'),
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
