use std::{
    collections::{HashMap, HashSet, VecDeque},
    env,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use base64::Engine;
use futures_util::StreamExt;
use miette::{Result, miette};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use tracing::warn;
use uuid::Uuid;

use crate::{
    config::{ModelConfig, redact_secret_text},
    context::Context,
    context_budget::{
        ContextBudgetExceededError, RequestBudgetLimits, estimate_agent_turn_request,
        estimate_prompt_request,
    },
    core::{Llm, TokenUsage, TokenUsageInfo},
    persistence::{PersistenceFileMode, PersistenceStore, write_bytes_atomic},
    providers::thinking::codex_reasoning_effort,
    reasoning::runtime::{
        AgentContent, AgentContentPart, AgentMessage, AgentToolCall, AgentToolInputSpec,
        AgentTurnItem, AgentTurnRequest, AgentTurnStreamResult, HistoryMessage, PromptRequest,
    },
    schema_utils::normalize_provider_function_schema,
};

use super::{
    default_rate_limit_backoff, format_request_error, looks_like_context_window_error,
    non_empty_string, shared_request_rate_limiter, summarize_agent_turn_request,
    summarize_prompt_request, take_next_sse_event, truncate_for_error, truncate_for_json_error,
};

const CODEX_OAUTH_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const CODEX_OAUTH_REFRESH_URL: &str = "https://auth.openai.com/oauth/token";
const CODEX_OAUTH_REFRESH_URL_OVERRIDE_ENV: &str = "CODEX_REFRESH_TOKEN_URL_OVERRIDE";
const CODEX_RESPONSES_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
const CODEX_CLIENT_VERSION: &str = "0.125.0";
const CODEX_CLIENT_VERSION_OVERRIDE_ENV: &str = "CODEX_CLIENT_VERSION_OVERRIDE";
const CODEX_ORIGINATOR: &str = "codex_cli_rs";
const CODEX_SESSION_ID_HEADER: &str = "session-id";
const CODEX_THREAD_ID_HEADER: &str = "thread-id";
const CODEX_WINDOW_ID_HEADER: &str = "x-codex-window-id";
const CODEX_HTTP_POOL_IDLE_TIMEOUT_SECS: u64 = 300;
const CODEX_HTTP_POOL_MAX_IDLE_PER_HOST: usize = 4;
const CODEX_HTTP_TCP_KEEPALIVE_SECS: u64 = 60;
const ACCESS_TOKEN_REFRESH_SKEW_MS: i64 = 60_000;

type RefreshLockMap = HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>;

static REFRESH_LOCKS_BY_AUTH_FILE: LazyLock<parking_lot::Mutex<RefreshLockMap>> =
    LazyLock::new(|| parking_lot::Mutex::new(HashMap::new()));

pub(crate) struct CodexOAuthClient {
    auth_file: PathBuf,
    auth_client: reqwest::Client,
    cached: tokio::sync::Mutex<Option<CodexOAuthAccess>>,
    inner: tokio::sync::Mutex<CodexResponsesClient>,
}

struct CodexResponsesClient {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
    extra_headers: reqwest::header::HeaderMap,
    model: String,
    thinking_budget: Option<String>,
    rpm: Option<usize>,
    stream_idle_timeout: Duration,
    context_window_tokens: usize,
    effective_context_window_tokens: usize,
    auto_compact_threshold_tokens: usize,
    reserved_output_tokens: usize,
    max_completion_tokens: usize,
    request_rate_limiter: Option<Arc<tokio::sync::Mutex<VecDeque<Instant>>>>,
    token_usage: std::sync::Mutex<TokenUsageInfo>,
    client_version: String,
    installation_id: String,
    /// Whether this model accepts image/vision input. Derived from config
    /// or catalog heuristic; can be set to `false` at runtime on error.
    supports_vision: std::sync::atomic::AtomicBool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CodexRequestIdentity {
    session_id: String,
    thread_id: String,
    window_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct CodexOAuthTokens {
    pub id_token: String,
    pub access_token: String,
    pub refresh_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    #[serde(default)]
    pub last_refresh_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CodexOAuthAccess {
    pub access_token: String,
    pub account_id: Option<String>,
    pub is_fedramp_account: bool,
    expires_at_ms: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct JwtPayload {
    #[serde(default)]
    exp: Option<i64>,
    #[serde(rename = "https://api.openai.com/auth", default)]
    auth: Option<OpenAiAuthClaims>,
}

#[derive(Debug, Deserialize)]
struct OpenAiAuthClaims {
    #[serde(default)]
    chatgpt_account_id: Option<String>,
    #[serde(default)]
    chatgpt_account_is_fedramp: bool,
}

#[derive(Serialize)]
struct RefreshRequest<'a> {
    client_id: &'static str,
    grant_type: &'static str,
    refresh_token: &'a str,
}

#[derive(Deserialize)]
struct RefreshResponse {
    id_token: Option<String>,
    access_token: Option<String>,
    refresh_token: Option<String>,
}

#[derive(Deserialize)]
struct CodexCliAuthFile {
    tokens: CodexCliAuthTokens,
}

#[derive(Deserialize)]
struct CodexCliAuthTokens {
    id_token: String,
    access_token: String,
    refresh_token: String,
    #[serde(default)]
    account_id: Option<String>,
}

impl CodexResponsesClient {
    fn new(base_url: &str, model_config: &ModelConfig) -> Self {
        let base_url = crate::config::normalize_provider_base_url(base_url);
        let request_timeout = Duration::from_secs(model_config.request_timeout_secs());
        let stream_idle_timeout = Duration::from_secs(model_config.stream_idle_timeout_secs());
        let client = codex_http_client_builder(request_timeout)
            .build()
            .expect("failed to build Codex Responses http client");
        let context_window_tokens = model_config.context_window_tokens();
        let effective_context_window_tokens = model_config.effective_context_window_tokens();
        let auto_compact_threshold_tokens = model_config.auto_compact_token_limit();
        let reserved_output_tokens = model_config.reserved_output_tokens();
        let max_completion_tokens = model_config.max_completion_tokens();
        let client_version = codex_oauth_client_version();
        let supports_vision_initial = {
            use crate::model_catalog::catalog_model_capacity;
            match model_config.supports_vision {
                Some(v) => v,
                None => catalog_model_capacity(&model_config.model_id)
                    .map(|c| c.supports_vision)
                    .unwrap_or(false),
            }
        };
        Self {
            client,
            api_key: "placeholder".to_string(),
            base_url: base_url.clone(),
            extra_headers: reqwest::header::HeaderMap::new(),
            model: model_config.model_id.clone(),
            thinking_budget: model_config
                .thinking_budget()
                .map(|budget| budget.as_str().to_string()),
            rpm: model_config.rpm(),
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
                model_context_window: Some(context_window_tokens as i64),
                daily_token_usage: Vec::new(),
            }),
            client_version,
            installation_id: Uuid::new_v4().to_string(),
            supports_vision: std::sync::atomic::AtomicBool::new(supports_vision_initial),
        }
    }

    fn set_auth(&mut self, access_token: String, extra_headers: reqwest::header::HeaderMap) {
        self.api_key = access_token;
        self.extra_headers = extra_headers;
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
                    "Codex Responses rpm throttle waiting {} ms before next request (rpm={})\n{}",
                    delay.as_millis(),
                    rpm,
                    request_context.join("\n")
                );
                logged_wait = true;
            }
            tokio::time::sleep(delay).await;
        }
    }

    async fn post_responses_with_retry(
        &self,
        payload: &Value,
        request_context: &[String],
        request_identity: Option<&CodexRequestIdentity>,
    ) -> Result<reqwest::Response> {
        const MAX_429_RETRIES: usize = 4;
        const MAX_5XX_RETRIES: usize = 3;

        let url = self.url();
        let mut rate_limit_attempt = 0usize;
        let mut transient_attempt = 0usize;
        loop {
            self.wait_for_request_slot(request_context).await;
            let mut request = self
                .client
                .post(&url)
                .bearer_auth(&self.api_key)
                .headers(self.extra_headers.clone())
                .header("version", &self.client_version)
                .header("originator", CODEX_ORIGINATOR);
            if let Some(identity) = request_identity {
                request = request
                    .header(CODEX_SESSION_ID_HEADER, &identity.session_id)
                    .header(CODEX_THREAD_ID_HEADER, &identity.thread_id)
                    .header(CODEX_WINDOW_ID_HEADER, &identity.window_id);
            }
            let response = request.json(payload).send().await.map_err(|err| {
                format_request_error(
                    "Codex Responses request failed",
                    &url,
                    request_context,
                    &err,
                )
            })?;

            if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
                let retry_after = response
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|value| value.to_str().ok())
                    .and_then(super::parse_retry_after_seconds);
                let body = response
                    .text()
                    .await
                    .map_err(|err| miette!("Codex Responses body read failed: {err}"))?;

                if rate_limit_attempt >= MAX_429_RETRIES {
                    return Err(miette!(
                        "Codex Responses returned HTTP 429 after {} retries: {}",
                        MAX_429_RETRIES,
                        truncate_for_error(&body)
                    ));
                }

                let delay = retry_after
                    .map(Duration::from_secs)
                    .unwrap_or_else(|| default_rate_limit_backoff(rate_limit_attempt));
                warn!(
                    "Codex Responses returned HTTP 429; retrying request in {} ms (attempt {}/{})\n{}",
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
                let body = response
                    .text()
                    .await
                    .map_err(|err| miette!("Codex Responses body read failed: {err}"))?;

                if transient_attempt >= MAX_5XX_RETRIES {
                    return Err(miette!(
                        "Codex Responses returned HTTP {} after {} retries: {}",
                        status,
                        MAX_5XX_RETRIES,
                        truncate_for_error(&body)
                    ));
                }

                let delay = Duration::from_millis(400 * (1u64 << transient_attempt));
                warn!(
                    "Codex Responses returned HTTP {}; retrying request in {} ms (attempt {}/{})\n{}",
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

    async fn run_json(&self, context: &Context, request: PromptRequest) -> Result<Value> {
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
        let request_identity = codex_request_identity(context);
        let payload =
            build_prompt_responses_payload(self, request, output_schema, request_identity.as_ref());
        let response = self
            .post_responses_with_retry(&payload, &request_context, request_identity.as_ref())
            .await?;
        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .await
                .map_err(|err| miette!("Codex Responses body read failed: {err}"))?;
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
            return Err(miette!(
                "Codex Responses returned HTTP {}: {}",
                status,
                truncate_for_error(&body)
            ));
        }

        let result = self
            .parse_responses_stream(Some(context), response, false)
            .await?;
        let content = result.last_assistant_message.as_deref().unwrap_or_default();
        if let Some(value) = super::extract_json_value_from_content(content) {
            return Ok(value);
        }
        Err(miette!(
            "Codex Responses JSON request did not return a JSON object; content={}",
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
        let request_context = summarize_agent_turn_request(&request, Some(&budget));
        use std::sync::atomic::Ordering;
        let strip_images = !self.supports_vision.load(Ordering::Relaxed);
        let request_identity = codex_request_identity(context);
        let payload = build_agent_responses_payload(
            self,
            request.clone(),
            strip_images,
            request_identity.as_ref(),
        );
        let response = self
            .post_responses_with_retry(&payload, &request_context, request_identity.as_ref())
            .await?;
        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .await
                .map_err(|err| miette!("Codex Responses body read failed: {err}"))?;
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
                    "Codex Responses rejected image input; retrying agent turn without images\n{}",
                    request_context.join("\n")
                );
                let payload = build_agent_responses_payload(
                    self,
                    request,
                    /*strip_images=*/ true,
                    request_identity.as_ref(),
                );
                let response = self
                    .post_responses_with_retry(
                        &payload,
                        &request_context,
                        request_identity.as_ref(),
                    )
                    .await?;
                let status = response.status();
                if status.is_success() {
                    return self
                        .parse_responses_stream(Some(context), response, true)
                        .await;
                }
                let body = response
                    .text()
                    .await
                    .map_err(|err| miette!("Codex Responses body read failed: {err}"))?;
                return Err(miette!(
                    "Codex Responses returned HTTP {}: {}",
                    status,
                    truncate_for_error(&body)
                ));
            }
            return Err(miette!(
                "Codex Responses returned HTTP {}: {}",
                status,
                truncate_for_error(&body)
            ));
        }
        self.parse_responses_stream(Some(context), response, true)
            .await
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

        while !completed {
            let next_chunk = tokio::time::timeout(self.stream_idle_timeout, stream.next())
                .await
                .map_err(|_| {
                    miette!(
                        "Codex Responses stream stalled for over {}s (model={}, url={})",
                        self.stream_idle_timeout.as_secs(),
                        self.model,
                        url
                    )
                })?;
            let Some(chunk) = next_chunk else {
                break;
            };
            let chunk =
                chunk.map_err(|err| miette!("Codex Responses stream read failed: {err}"))?;
            buffer.push_str(&String::from_utf8_lossy(&chunk));
            super::normalize_sse_buffer(&mut buffer);

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
                        "Codex Responses stream event is not valid JSON: {err}; data={}",
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
                            "Codex Responses stream failed: {}",
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

    fn record_last_usage(&self, usage: TokenUsage) {
        if let Ok(mut info) = self.token_usage.lock() {
            info.model_context_window = Some(self.context_window_tokens as i64);
            info.append_last_usage(usage);
        }
    }

    fn token_usage_info(&self) -> Option<TokenUsageInfo> {
        self.token_usage.lock().ok().map(|info| info.clone())
    }

    fn model_name(&self) -> Option<String> {
        Some(self.model.clone())
    }
}

fn codex_http_client_builder(timeout: Duration) -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .timeout(timeout)
        .pool_idle_timeout(Duration::from_secs(CODEX_HTTP_POOL_IDLE_TIMEOUT_SECS))
        .pool_max_idle_per_host(CODEX_HTTP_POOL_MAX_IDLE_PER_HOST)
        .tcp_keepalive(Duration::from_secs(CODEX_HTTP_TCP_KEEPALIVE_SECS))
}

impl CodexOAuthClient {
    pub(crate) fn new(
        provider_name: &str,
        auth_file: Option<&str>,
        base_url: Option<&str>,
        model_config: &ModelConfig,
    ) -> Self {
        let auth_file = codex_oauth_auth_file(provider_name, auth_file);
        let auth_client = codex_http_client_builder(Duration::from_secs(15))
            .build()
            .expect("failed to build OpenAI Codex auth http client");
        let base_url = base_url.unwrap_or(CODEX_RESPONSES_BASE_URL);
        let inner = CodexResponsesClient::new(base_url, model_config);
        Self {
            auth_file,
            auth_client,
            cached: tokio::sync::Mutex::new(None),
            inner: tokio::sync::Mutex::new(inner),
        }
    }

    async fn ensure_auth(&self) -> Result<()> {
        let access = codex_oauth_access_from_file_with_client(&self.auth_file, &self.auth_client)
            .await
            .map_err(|err| {
                miette!(
                    "OpenAI Codex auth at {} is unavailable: {err}",
                    self.auth_file.display()
                )
            })?;

        let unchanged = {
            let cached = self.cached.lock().await;
            cached.as_ref() == Some(&access)
        };
        if unchanged {
            return Ok(());
        }

        let mut headers = reqwest::header::HeaderMap::new();
        if let Some(account_id) = &access.account_id {
            headers.insert(
                "ChatGPT-Account-ID",
                account_id
                    .parse()
                    .map_err(|err| miette!("invalid ChatGPT account id header: {err}"))?,
            );
        }
        if access.is_fedramp_account {
            headers.insert(
                "X-OpenAI-Fedramp",
                reqwest::header::HeaderValue::from_static("true"),
            );
        }

        let mut inner = self.inner.lock().await;
        inner.set_auth(access.access_token.clone(), headers);
        *self.cached.lock().await = Some(access);
        Ok(())
    }
}

#[async_trait]
impl Llm for CodexOAuthClient {
    async fn run_json(
        &self,
        context: &Context,
        request: PromptRequest,
    ) -> Result<serde_json::Value> {
        self.ensure_auth().await?;
        self.inner.lock().await.run_json(context, request).await
    }

    async fn run_agent_turn(
        &self,
        context: &Context,
        request: AgentTurnRequest,
    ) -> Result<AgentTurnStreamResult> {
        self.ensure_auth().await?;
        self.inner
            .lock()
            .await
            .run_agent_turn(context, request)
            .await
    }

    fn token_usage_info(&self) -> Option<TokenUsageInfo> {
        self.inner.try_lock().ok()?.token_usage_info()
    }

    fn model_name(&self) -> Option<String> {
        self.inner.try_lock().ok()?.model_name()
    }
}

fn build_prompt_responses_payload(
    client: &CodexResponsesClient,
    request: PromptRequest,
    output_schema: Value,
    request_identity: Option<&CodexRequestIdentity>,
) -> Value {
    let messages = request.all_messages();
    // Prompt requests are structured output calls; pass strip_images=false as
    // prompts don't carry user-uploaded image attachments.
    let (instructions, input) = history_messages_to_responses_parts(messages, false);
    let mut payload = base_responses_payload(
        client,
        instructions,
        input,
        Vec::new(),
        request_identity,
        "prompt",
    );
    payload["text"] = json!({
        "format": {
            "type": "json_schema",
            "name": sanitize_text_format_name(&request.tool_name),
            "strict": true,
            "schema": output_schema,
        }
    });
    payload
}

fn build_agent_responses_payload(
    client: &CodexResponsesClient,
    request: AgentTurnRequest,
    strip_images: bool,
    request_identity: Option<&CodexRequestIdentity>,
) -> Value {
    let (instructions, input) = agent_messages_to_responses_parts(request.messages, strip_images);
    let tools = request
        .tools
        .into_iter()
        .map(agent_tool_to_responses_tool)
        .collect::<Vec<_>>();
    base_responses_payload(
        client,
        instructions,
        input,
        tools,
        request_identity,
        "agent",
    )
}

fn base_responses_payload(
    client: &CodexResponsesClient,
    instructions: String,
    input: Vec<Value>,
    tools: Vec<Value>,
    request_identity: Option<&CodexRequestIdentity>,
    request_kind: &str,
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
        "max_output_tokens": client.max_completion_tokens,
        "include": [],
        "client_metadata": {
            "x-codex-installation-id": client.installation_id,
        },
    });
    if let Some(identity) = request_identity {
        payload["prompt_cache_key"] = json!(prompt_cache_key(identity, request_kind));
        payload["client_metadata"][CODEX_WINDOW_ID_HEADER] = json!(identity.window_id);
    }
    if let Some(budget) = client.thinking_budget.as_deref() {
        payload["reasoning"] = json!({ "effort": codex_reasoning_effort(budget) });
    }
    payload
}

fn codex_request_identity(context: &Context) -> Option<CodexRequestIdentity> {
    context
        .session_id
        .as_deref()
        .map(|session_id| CodexRequestIdentity {
            session_id: session_id.to_string(),
            thread_id: session_id.to_string(),
            window_id: format!("{session_id}:0"),
        })
}

fn prompt_cache_key(identity: &CodexRequestIdentity, request_kind: &str) -> String {
    format!("daat-locus:{}:{request_kind}", identity.session_id)
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
                        super::flatten_tool_result_as_assistant_text(&name, &content),
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
                let Some(url) = super::payload::image_part_data_url(part) else {
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

fn agent_tool_to_responses_tool(tool: crate::reasoning::runtime::AgentToolSpec) -> Value {
    match tool.input_spec {
        AgentToolInputSpec::JsonSchema { schema } => json!({
            "type": "function",
            "name": tool.name,
            "description": tool.description,
            "strict": true,
            "parameters": normalize_provider_function_schema(schema),
        }),
        AgentToolInputSpec::FreeformGrammar {
            syntax,
            definition,
            fallback_schema,
        } => {
            if codex_responses_supports_custom_tool_grammar(&syntax) {
                json!({
                    "type": "custom",
                    "name": tool.name,
                    "description": tool.description,
                    "format": {
                        "type": "grammar",
                        "syntax": syntax,
                        "definition": definition,
                    },
                })
            } else {
                json!({
                    "type": "function",
                    "name": tool.name,
                    "description": format!(
                        "{}\n\nThis is a FREEFORM grammar tool. Codex Responses only accepts `lark` or `regex` custom tool grammars, so this provider falls back to single-string input: put the complete tool input in the `input` field.\nsyntax={syntax}\ndefinition=\n{definition}",
                        tool.description
                    ),
                    "strict": false,
                    "parameters": normalize_provider_function_schema(fallback_schema),
                })
            }
        }
    }
}

fn codex_responses_supports_custom_tool_grammar(syntax: &str) -> bool {
    matches!(syntax, "lark" | "regex")
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
                        "Codex Responses function_call missing call_id; item={}",
                        truncate_for_json_error(item)
                    )
                })?;
            let name = item.get("name").and_then(Value::as_str).ok_or_else(|| {
                miette!(
                    "Codex Responses function_call missing name; item={}",
                    truncate_for_json_error(item)
                )
            })?;
            let arguments_str = item
                .get("arguments")
                .and_then(Value::as_str)
                .unwrap_or("{}");
            let arguments = serde_json::from_str(arguments_str).map_err(|err| {
                miette!(
                    "failed to decode Codex Responses function_call arguments as JSON: {err}; arguments={}",
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
                        "Codex Responses custom_tool_call missing call_id; item={}",
                        truncate_for_json_error(item)
                    )
                })?;
            let name = item.get("name").and_then(Value::as_str).ok_or_else(|| {
                miette!(
                    "Codex Responses custom_tool_call missing name; item={}",
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

pub(crate) fn codex_oauth_auth_file(provider_name: &str, auth_file: Option<&str>) -> PathBuf {
    let Some(auth_file) = auth_file.map(str::trim).filter(|value| !value.is_empty()) else {
        return default_codex_oauth_auth_file(provider_name);
    };
    let path = PathBuf::from(auth_file);
    if path.is_absolute() {
        path
    } else {
        PersistenceStore::runtime_sync().config_file(auth_file)
    }
}

pub(crate) fn codex_oauth_default_base_url() -> &'static str {
    CODEX_RESPONSES_BASE_URL
}

pub(crate) fn codex_oauth_client_version() -> String {
    std::env::var(CODEX_CLIENT_VERSION_OVERRIDE_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| CODEX_CLIENT_VERSION.to_string())
}

pub(crate) fn default_codex_oauth_auth_file(provider_name: &str) -> PathBuf {
    let file_name = format!(
        "openai-codex-oauth-{}.json",
        sanitize_auth_file_component(provider_name)
    );
    PersistenceStore::runtime_sync().config_file(&file_name)
}

pub(crate) fn codex_cli_auth_file() -> PathBuf {
    env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            env::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".codex")
        })
        .join("auth.json")
}

fn sanitize_auth_file_component(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
        } else {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "provider".to_string()
    } else {
        trimmed.to_string()
    }
}

pub(crate) async fn write_codex_oauth_tokens(
    auth_file: &Path,
    tokens: &CodexOAuthTokens,
) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(tokens)
        .map_err(|err| miette!("serialize OpenAI Codex tokens failed: {err}"))?;
    write_bytes_atomic(auth_file.to_path_buf(), bytes, PersistenceFileMode::Private)
        .await
        .map_err(|err| {
            miette!(
                "write OpenAI Codex tokens {} failed: {err}",
                auth_file.display()
            )
        })
}

pub(crate) async fn codex_oauth_access_from_file(auth_file: &Path) -> Result<CodexOAuthAccess> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|err| miette!("failed to build OpenAI Codex auth http client: {err}"))?;
    codex_oauth_access_from_file_with_client(auth_file, &client).await
}

pub(crate) async fn import_codex_cli_oauth_tokens(auth_file: &Path) -> Result<CodexOAuthTokens> {
    let bytes = tokio::fs::read(auth_file)
        .await
        .map_err(|err| miette!("read Codex auth file {} failed: {err}", auth_file.display()))?;
    parse_codex_cli_oauth_tokens(&bytes).map_err(|err| {
        miette!(
            "parse Codex auth file {} failed: {err}",
            auth_file.display()
        )
    })
}

fn parse_codex_cli_oauth_tokens(bytes: &[u8]) -> Result<CodexOAuthTokens> {
    let auth: CodexCliAuthFile = serde_json::from_slice(bytes).map_err(|err| {
        miette!(
            "expected Codex CLI auth.json with a tokens object containing id_token, access_token, and refresh_token: {err}"
        )
    })?;
    Ok(CodexOAuthTokens {
        id_token: auth.tokens.id_token,
        access_token: auth.tokens.access_token,
        refresh_token: auth.tokens.refresh_token,
        account_id: auth.tokens.account_id,
        last_refresh_at_ms: now_ms(),
    })
}

async fn codex_oauth_access_from_file_with_client(
    auth_file: &Path,
    client: &reqwest::Client,
) -> Result<CodexOAuthAccess> {
    let mut tokens = read_codex_oauth_tokens(auth_file).await?;
    if codex_oauth_tokens_need_refresh(&tokens) {
        let refresh_lock = refresh_lock_for_auth_file(auth_file);
        let _guard = refresh_lock.lock().await;
        tokens = read_codex_oauth_tokens(auth_file).await?;
        if !codex_oauth_tokens_need_refresh(&tokens) {
            return codex_oauth_access_from_tokens(&tokens);
        }
        tokens = refresh_codex_oauth_tokens(client, &tokens).await?;
        write_codex_oauth_tokens(auth_file, &tokens).await?;
    }
    codex_oauth_access_from_tokens(&tokens)
}

fn refresh_lock_for_auth_file(auth_file: &Path) -> Arc<tokio::sync::Mutex<()>> {
    let mut locks = REFRESH_LOCKS_BY_AUTH_FILE.lock();
    locks
        .entry(auth_file.to_path_buf())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

async fn read_codex_oauth_tokens(auth_file: &Path) -> Result<CodexOAuthTokens> {
    let bytes = tokio::fs::read(auth_file)
        .await
        .map_err(|err| miette!("read {} failed: {err}", auth_file.display()))?;
    serde_json::from_slice(&bytes)
        .map_err(|err| miette!("parse {} failed: {err}", auth_file.display()))
}

fn codex_oauth_tokens_need_refresh(tokens: &CodexOAuthTokens) -> bool {
    let Some(expires_at_ms) = access_token_expires_at_ms(&tokens.access_token) else {
        return false;
    };
    now_ms().saturating_add(ACCESS_TOKEN_REFRESH_SKEW_MS) >= expires_at_ms
}

async fn refresh_codex_oauth_tokens(
    client: &reqwest::Client,
    tokens: &CodexOAuthTokens,
) -> Result<CodexOAuthTokens> {
    let endpoint = std::env::var(CODEX_OAUTH_REFRESH_URL_OVERRIDE_ENV)
        .unwrap_or_else(|_| CODEX_OAUTH_REFRESH_URL.to_string());
    let response = client
        .post(&endpoint)
        .header("Content-Type", "application/json")
        .json(&RefreshRequest {
            client_id: CODEX_OAUTH_CLIENT_ID,
            grant_type: "refresh_token",
            refresh_token: &tokens.refresh_token,
        })
        .send()
        .await
        .map_err(|err| miette!("OpenAI Codex token refresh request failed: {err}"))?;

    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|err| miette!("OpenAI Codex token refresh body read failed: {err}"))?;
    if !status.is_success() {
        let body = redact_secret_text(&body, &tokens.refresh_token);
        return Err(miette!(
            "OpenAI Codex token refresh returned HTTP {status}: {body}"
        ));
    }

    let refreshed: RefreshResponse = serde_json::from_str(&body)
        .map_err(|err| miette!("OpenAI Codex token refresh JSON parse failed: {err}"))?;
    let mut next = tokens.clone();
    if let Some(id_token) = refreshed.id_token {
        next.id_token = id_token;
    }
    if let Some(access_token) = refreshed.access_token {
        next.access_token = access_token;
    }
    if let Some(refresh_token) = refreshed.refresh_token {
        next.refresh_token = refresh_token;
    }
    next.last_refresh_at_ms = now_ms();
    if let Some(account_id) =
        jwt_openai_auth_claims(&next.id_token).and_then(|claims| claims.chatgpt_account_id)
    {
        next.account_id = Some(account_id);
    }
    Ok(next)
}

pub(crate) fn codex_oauth_access_from_tokens(
    tokens: &CodexOAuthTokens,
) -> Result<CodexOAuthAccess> {
    let claims = jwt_openai_auth_claims(&tokens.id_token);
    Ok(CodexOAuthAccess {
        access_token: tokens.access_token.clone(),
        account_id: tokens.account_id.clone().or_else(|| {
            claims
                .as_ref()
                .and_then(|claims| claims.chatgpt_account_id.clone())
        }),
        is_fedramp_account: claims.is_some_and(|claims| claims.chatgpt_account_is_fedramp),
        expires_at_ms: access_token_expires_at_ms(&tokens.access_token),
    })
}

fn access_token_expires_at_ms(access_token: &str) -> Option<i64> {
    let payload = decode_jwt_payload::<JwtPayload>(access_token).ok()?;
    payload.exp.map(|exp_secs| exp_secs.saturating_mul(1000))
}

fn jwt_openai_auth_claims(jwt: &str) -> Option<OpenAiAuthClaims> {
    decode_jwt_payload::<JwtPayload>(jwt).ok()?.auth
}

fn decode_jwt_payload<T: DeserializeOwned>(jwt: &str) -> Result<T> {
    let mut parts = jwt.split('.');
    let (_header, payload, _signature) = match (parts.next(), parts.next(), parts.next()) {
        (Some(header), Some(payload), Some(signature))
            if !header.is_empty() && !payload.is_empty() && !signature.is_empty() =>
        {
            (header, payload, signature)
        }
        _ => return Err(miette!("invalid JWT format")),
    };
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(payload))
        .map_err(|err| miette!("invalid JWT base64 payload: {err}"))?;
    serde_json::from_slice(&bytes).map_err(|err| miette!("invalid JWT JSON payload: {err}"))
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json_contains_key(value: &Value, needle: &str) -> bool {
        match value {
            Value::Object(object) => {
                object.contains_key(needle)
                    || object
                        .values()
                        .any(|value| json_contains_key(value, needle))
            }
            Value::Array(values) => values.iter().any(|value| json_contains_key(value, needle)),
            _ => false,
        }
    }

    fn thinking_budget(value: &str) -> ThinkingBudget {
        serde_json::from_value(serde_json::json!(value)).expect("thinking budget deserializes")
    }
    use crate::config::{ModelConfig, ThinkingBudget};

    fn jwt(payload: serde_json::Value) -> String {
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#);
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&payload).unwrap());
        format!("{header}.{payload}.sig")
    }

    #[test]
    fn parse_codex_cli_auth_file_extracts_nested_tokens() {
        let before = now_ms();
        let tokens = parse_codex_cli_oauth_tokens(
            br#"{
                "auth_mode": "chatgpt",
                "tokens": {
                    "id_token": "id-token",
                    "access_token": "access-token",
                    "refresh_token": "refresh-token",
                    "account_id": "account-123"
                },
                "last_refresh": "2026-06-10T00:00:00Z"
            }"#,
        )
        .expect("parse Codex CLI auth");

        assert_eq!(tokens.id_token, "id-token");
        assert_eq!(tokens.access_token, "access-token");
        assert_eq!(tokens.refresh_token, "refresh-token");
        assert_eq!(tokens.account_id.as_deref(), Some("account-123"));
        assert!(tokens.last_refresh_at_ms >= before);
    }

    #[test]
    fn codex_oauth_access_extracts_account_headers_from_id_token() {
        let id_token = jwt(serde_json::json!({
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "account-123",
                "chatgpt_account_is_fedramp": true
            }
        }));
        let access_token = jwt(serde_json::json!({
            "exp": 4_102_444_800_i64
        }));
        let tokens = CodexOAuthTokens {
            id_token,
            access_token: access_token.clone(),
            refresh_token: "refresh".to_string(),
            account_id: None,
            last_refresh_at_ms: 0,
        };

        let access = codex_oauth_access_from_tokens(&tokens).unwrap();

        assert_eq!(access.access_token, access_token);
        assert_eq!(access.account_id.as_deref(), Some("account-123"));
        assert!(access.is_fedramp_account);
        assert_eq!(access.expires_at_ms, Some(4_102_444_800_000));
    }

    #[test]
    fn codex_oauth_configured_account_id_overrides_id_token_claim() {
        let id_token = jwt(serde_json::json!({
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "claim-account"
            }
        }));
        let tokens = CodexOAuthTokens {
            id_token,
            access_token: "opaque".to_string(),
            refresh_token: "refresh".to_string(),
            account_id: Some("configured-account".to_string()),
            last_refresh_at_ms: 0,
        };

        let access = codex_oauth_access_from_tokens(&tokens).unwrap();

        assert_eq!(access.account_id.as_deref(), Some("configured-account"));
    }

    #[test]
    fn default_auth_file_sanitizes_provider_name() {
        assert_eq!(
            sanitize_auth_file_component("OpenAI/Codex OAuth!"),
            "OpenAI-Codex-OAuth"
        );
    }

    fn test_client() -> CodexResponsesClient {
        CodexResponsesClient::new(
            CODEX_RESPONSES_BASE_URL,
            &ModelConfig {
                model_id: "gpt-5.4".to_string(),
                provider: "codex-oauth".to_string(),
                ..ModelConfig::default()
            },
        )
    }

    fn test_identity() -> CodexRequestIdentity {
        CodexRequestIdentity {
            session_id: "session-test".to_string(),
            thread_id: "session-test".to_string(),
            window_id: "session-test:0".to_string(),
        }
    }

    #[test]
    fn codex_request_budget_reserves_effective_window_headroom() {
        let client = CodexResponsesClient::new(
            CODEX_RESPONSES_BASE_URL,
            &ModelConfig {
                model_id: "gpt-5.5".to_string(),
                provider: "codex-oauth".to_string(),
                context_window_tokens: 272_000,
                effective_context_window_percent: 95,
                max_completion_tokens: 128_000,
                ..ModelConfig::default()
            },
        );
        let limits = client.request_budget_limits();

        assert_eq!(limits.context_window_tokens, 258_400);
        assert_eq!(limits.auto_compact_threshold_tokens, 244_800);
        assert_eq!(limits.reserved_output_tokens, 13_600);
    }

    #[test]
    fn codex_thinking_budget_max_maps_to_xhigh() {
        let client = CodexResponsesClient::new(
            CODEX_RESPONSES_BASE_URL,
            &ModelConfig {
                model_id: "gpt-5.4".to_string(),
                provider: "codex-oauth".to_string(),
                thinking_budget: Some(thinking_budget("max")),
                ..ModelConfig::default()
            },
        );

        let payload = base_responses_payload(
            &client,
            "instructions".to_string(),
            vec![],
            vec![],
            None,
            "agent",
        );

        assert_eq!(payload["reasoning"]["effort"], "xhigh");
    }

    #[test]
    fn terminal_write_stdin_schema_sent_to_codex_has_no_one_of() {
        let tool = crate::reasoning::runtime::AgentToolSpec {
            name: "terminal__terminal_write_stdin".to_string(),
            description: "Continue terminal session".to_string(),
            input_spec: AgentToolInputSpec::JsonSchema {
                schema: serde_json::to_value(schemars::schema_for!(
                    crate::core::TerminalWriteStdinArgs
                ))
                .unwrap(),
            },
        };

        let payload = agent_tool_to_responses_tool(tool);

        assert!(
            !json_contains_key(&payload["parameters"], "oneOf"),
            "{payload:#}"
        );
    }

    #[test]
    fn agent_payload_preserves_responses_tool_call_history() {
        let client = test_client();
        let request = AgentTurnRequest {
            messages: vec![
                AgentMessage::system("base instructions"),
                AgentMessage::user("do work"),
                AgentMessage::assistant_tool_call_protocol_with_reasoning(
                    None,
                    None,
                    vec![AgentToolCall {
                        id: "call-1".to_string(),
                        name: "update_plan".to_string(),
                        arguments: json!({"plan": []}),
                    }],
                ),
                AgentMessage::tool("call-1", "update_plan", "plan updated"),
            ],
            tools: vec![
                crate::reasoning::runtime::AgentToolSpec {
                    name: "update_plan".to_string(),
                    description: "Update plan".to_string(),
                    input_spec: AgentToolInputSpec::JsonSchema {
                        schema: json!({
                            "type": "object",
                            "properties": {"plan": {"type": "array"}},
                            "required": ["plan"],
                            "additionalProperties": false
                        }),
                    },
                },
                crate::reasoning::runtime::AgentToolSpec {
                    name: "custom_patch".to_string(),
                    description: "Apply custom patch".to_string(),
                    input_spec: AgentToolInputSpec::FreeformGrammar {
                        syntax: "unified_diff".to_string(),
                        definition: "patch := text".to_string(),
                        fallback_schema: json!({
                            "type": "object",
                            "properties": {
                                "input": {
                                    "type": "string"
                                }
                            },
                            "required": ["input"],
                            "additionalProperties": false
                        }),
                    },
                },
            ],
        };

        let payload = build_agent_responses_payload(&client, request, false, None);

        assert_eq!(payload["instructions"], "base instructions");
        assert_eq!(payload["input"][1]["type"], "function_call");
        assert_eq!(payload["input"][1]["call_id"], "call-1");
        assert_eq!(payload["input"][1]["arguments"], "{\"plan\":[]}");
        assert_eq!(payload["input"][2]["type"], "function_call_output");
        assert_eq!(payload["input"][2]["output"], "plan updated");
        assert_eq!(payload["tools"][0]["type"], "function");
        assert_eq!(payload["tools"][0]["name"], "update_plan");
        assert_eq!(payload["tools"][1]["type"], "function");
        assert_eq!(payload["tools"][1]["name"], "custom_patch");
        assert_eq!(payload["tools"][1]["strict"], false);
        assert_eq!(
            payload["tools"][1]["parameters"]["properties"]["input"]["type"],
            "string"
        );
        assert!(
            payload["tools"][1]["description"]
                .as_str()
                .unwrap()
                .contains("syntax=unified_diff")
        );
    }

    #[test]
    fn agent_payload_prompt_cache_key_is_session_scoped() {
        let client = test_client();
        let request = AgentTurnRequest {
            messages: vec![AgentMessage::system("base"), AgentMessage::user("work")],
            tools: Vec::new(),
        };
        let identity = test_identity();
        let cache_key = "daat-locus:session-test:agent";

        let first = build_agent_responses_payload(&client, request.clone(), false, Some(&identity));
        let second = build_agent_responses_payload(&client, request, false, Some(&identity));

        assert_eq!(first["prompt_cache_key"], second["prompt_cache_key"]);
        assert_eq!(first["prompt_cache_key"], cache_key);
        assert_eq!(
            first["client_metadata"][CODEX_WINDOW_ID_HEADER].as_str(),
            Some(identity.window_id.as_str())
        );
        assert_eq!(
            first["client_metadata"]["x-codex-installation-id"].as_str(),
            Some(client.installation_id.as_str())
        );
    }

    #[test]
    fn agent_payload_omits_prompt_cache_key_without_session_scope() {
        let client = test_client();
        let request = AgentTurnRequest {
            messages: vec![AgentMessage::system("base"), AgentMessage::user("work")],
            tools: Vec::new(),
        };

        let payload = build_agent_responses_payload(&client, request, false, None);

        assert!(payload.get("prompt_cache_key").is_none());
    }

    #[test]
    fn agent_payload_keeps_previous_turn_input_as_next_prefix() {
        let client = test_client();
        let identity = test_identity();
        let tools = vec![crate::reasoning::runtime::AgentToolSpec {
            name: "terminal_exec".to_string(),
            description: "Run a command".to_string(),
            input_spec: AgentToolInputSpec::JsonSchema {
                schema: json!({
                    "type": "object",
                    "properties": {"command": {"type": "string"}},
                    "required": ["command"],
                    "additionalProperties": false
                }),
            },
        }];
        let first_messages = vec![
            AgentMessage::system("stable runtime instructions"),
            AgentMessage::user("<afterclaim_context>claimed input</afterclaim_context>"),
            AgentMessage::user("<preturn_context>state one</preturn_context>"),
        ];
        let mut second_messages = first_messages.clone();
        second_messages.push(AgentMessage::assistant_tool_call_protocol_with_reasoning(
            None,
            None,
            vec![AgentToolCall {
                id: "call-1".to_string(),
                name: "terminal_exec".to_string(),
                arguments: json!({"command": "pwd"}),
            }],
        ));
        second_messages.push(AgentMessage::tool(
            "call-1",
            "terminal_exec",
            "summary=ran pwd",
        ));
        second_messages.push(AgentMessage::user(
            "<preturn_context>state two</preturn_context>",
        ));

        let first = build_agent_responses_payload(
            &client,
            AgentTurnRequest {
                messages: first_messages,
                tools: tools.clone(),
            },
            false,
            Some(&identity),
        );
        let second = build_agent_responses_payload(
            &client,
            AgentTurnRequest {
                messages: second_messages,
                tools,
            },
            false,
            Some(&identity),
        );

        let first_input = first["input"].as_array().expect("first input");
        let second_input = second["input"].as_array().expect("second input");
        assert_eq!(first["instructions"], second["instructions"]);
        assert_eq!(first["tools"], second["tools"]);
        assert!(second_input.starts_with(first_input));
        assert_eq!(
            second_input
                .last()
                .and_then(|value| value.pointer("/content/0/text")),
            Some(&json!("<preturn_context>state two</preturn_context>"))
        );
    }

    #[test]
    fn agent_payload_serializes_multimodal_user_content() {
        let client = test_client();
        let dir = tempfile::tempdir().unwrap();
        let image_path = dir.path().join("sample.png");
        std::fs::write(&image_path, b"png-bytes").unwrap();
        let request = AgentTurnRequest {
            messages: vec![AgentMessage::user_content(AgentContent::multimodal(
                "describe this",
                vec![AgentContentPart::Image {
                    path: image_path.display().to_string(),
                    media_type: "application/octet-stream".to_string(),
                    description: Some("sample".to_string()),
                }],
            ))],
            tools: vec![],
        };

        let payload = build_agent_responses_payload(&client, request, false, None);

        assert_eq!(payload["input"][0]["role"], "user");
        assert_eq!(payload["input"][0]["content"][0]["type"], "input_text");
        assert_eq!(payload["input"][0]["content"][0]["text"], "describe this");
        assert_eq!(payload["input"][0]["content"][1]["type"], "input_image");
        assert!(
            payload["input"][0]["content"][1]["image_url"]
                .as_str()
                .unwrap()
                .starts_with("data:image/png;base64,")
        );
    }

    #[test]
    fn supported_freeform_grammars_use_custom_tools() {
        let client = test_client();
        let request = AgentTurnRequest {
            messages: vec![AgentMessage::user("do work")],
            tools: vec![crate::reasoning::runtime::AgentToolSpec {
                name: "structured_patch".to_string(),
                description: "Apply structured patch".to_string(),
                input_spec: AgentToolInputSpec::FreeformGrammar {
                    syntax: "lark".to_string(),
                    definition: "start: /.+/".to_string(),
                    fallback_schema: json!({
                        "type": "object",
                        "properties": {
                            "input": {
                                "type": "string"
                            }
                        },
                        "required": ["input"],
                        "additionalProperties": false
                    }),
                },
            }],
        };

        let payload = build_agent_responses_payload(&client, request, false, None);

        assert_eq!(payload["tools"][0]["type"], "custom");
        assert_eq!(payload["tools"][0]["format"]["type"], "grammar");
        assert_eq!(payload["tools"][0]["format"]["syntax"], "lark");
        assert_eq!(payload["tools"][0]["format"]["definition"], "start: /.+/");
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
            "name": "finish_and_send",
            "arguments": "{\"reply_message\":\"ok\"}"
        });
        let parsed = response_item_tool_call(&call).unwrap().unwrap();
        assert_eq!(parsed.id, "call-2");
        assert_eq!(parsed.name, "finish_and_send");
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
}
