use std::{
    collections::{HashMap, VecDeque},
    env,
    path::{Path, PathBuf},
    sync::{
        Arc, LazyLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use base64::Engine;
use miette::{Result, miette};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tracing::warn;
use uuid::Uuid;

use crate::{
    config::{ModelConfig, redact_secret_text},
    context_budget::{ContextBudgetExceededError, RequestBudgetLimits},
    core::{ModelProgressSink, ModelProvider, ModelRequestOptions, TokenUsage, TokenUsageInfo},
    daat_locus_paths::daat_locus_paths_sync,
    model_protocol::{AgentTurnRequest, AgentTurnStreamResult, PromptRequest},
    persistence::{PersistenceFileMode, write_bytes_atomic},
};

use super::responses_protocol::{
    history_messages_to_responses_parts, messages_to_responses_parts, reasoning_payload,
    sanitize_text_format_name, tool_to_responses_tool,
};
use super::{
    default_rate_limit_backoff, looks_like_context_window_error,
    read_response_text_with_idle_timeout, send_request_for_streaming_response,
    shared_request_rate_limiter, summarize_agent_turn_request, summarize_prompt_request,
    truncate_for_error,
};

const CODEX_OAUTH_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const CODEX_OAUTH_REFRESH_URL: &str = "https://auth.openai.com/oauth/token";
const CODEX_OAUTH_REFRESH_URL_OVERRIDE_ENV: &str = "CODEX_REFRESH_TOKEN_URL_OVERRIDE";
const CODEX_RESPONSES_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
const CODEX_CLIENT_VERSION: &str = "0.162.0";
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

pub struct CodexClient {
    auth_file: Option<PathBuf>,
    auth_client: reqwest::Client,
    cached: tokio::sync::Mutex<Option<CodexOAuthAccess>>,
    inner: tokio::sync::Mutex<CodexResponsesClient>,
    request_budget_limits: RequestBudgetLimits,
    model: String,
}

struct CodexResponsesClient {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
    extra_headers: reqwest::header::HeaderMap,
    model: String,
    max_output_tokens: Option<usize>,
    thinking_budget: Option<String>,
    rpm: Option<usize>,
    header_timeout: Option<Duration>,
    stream_idle_timeout: Option<Duration>,
    context_window_tokens: usize,
    effective_context_window_tokens: usize,
    auto_compact_threshold_tokens: usize,
    reserved_output_tokens: usize,
    request_rate_limiter: Option<Arc<tokio::sync::Mutex<VecDeque<Instant>>>>,
    token_usage: std::sync::Mutex<TokenUsageInfo>,
    client_version: String,
    installation_id: String,
    /// Whether this model accepts image/vision input. Derived from config
    /// or catalog heuristic; can be set to `false` at runtime on error.
    supports_vision: AtomicBool,
    /// Whether this model/account accepts `reasoning.summary`.
    /// It is an optional display enhancement, so runtime errors disable it.
    supports_reasoning_summary: AtomicBool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CodexRequestIdentity {
    session_id: String,
    thread_id: String,
    window_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct CodexOAuthTokens {
    pub id_token: String,
    pub access_token: String,
    pub refresh_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    #[serde(default)]
    pub last_refresh_at_ms: i64,
}

#[derive(Deserialize, Serialize)]
struct StoredCodexAuth {
    #[serde(flatten)]
    tokens: CodexOAuthTokens,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    imported_from: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodexOAuthAccess {
    pub access_token: String,
    pub account_id: Option<String>,
    pub is_fedramp_account: bool,
    expires_at_ms: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct JwtPayload {
    #[serde(default)]
    sub: Option<String>,
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
    #[serde(default)]
    tokens: Option<CodexCliAuthTokens>,
    #[serde(rename = "OPENAI_API_KEY", default)]
    openai_api_key: Option<String>,
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
        let header_timeout = model_config.response_header_timeout();
        let stream_idle_timeout = model_config.stream_idle_timeout();
        let client = codex_responses_http_client_builder()
            .build()
            .expect("failed to build Codex Responses http client");
        let context_window_tokens = model_config.context_window_tokens();
        let effective_context_window_tokens = model_config.effective_context_window_tokens();
        let auto_compact_threshold_tokens = model_config.auto_compact_token_limit();
        let reserved_output_tokens = model_config.reserved_output_tokens();
        let client_version = codex_oauth_client_version();
        let supports_vision_initial = {
            use crate::model_catalog::catalog_model_capacity;
            model_config.supports_vision.unwrap_or_else(|| {
                catalog_model_capacity(&model_config.model_id).is_some_and(|c| c.supports_vision)
            })
        };
        Self {
            client,
            api_key: "placeholder".to_string(),
            base_url: base_url.clone(),
            extra_headers: reqwest::header::HeaderMap::new(),
            model: model_config.model_id.clone(),
            max_output_tokens: None,
            thinking_budget: model_config
                .thinking_budget()
                .map(|budget| budget.as_str().to_string()),
            rpm: model_config.rpm(),
            header_timeout,
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
                model_context_window: i64::try_from(context_window_tokens).ok(),
                daily_token_usage: Vec::new(),
            }),
            client_version,
            installation_id: Uuid::new_v4().to_string(),
            supports_vision: AtomicBool::new(supports_vision_initial),
            supports_reasoning_summary: AtomicBool::new(true),
        }
    }

    fn set_auth(&mut self, access_token: String, extra_headers: reqwest::header::HeaderMap) {
        self.api_key = access_token;
        self.extra_headers = extra_headers;
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
        crate::image_input::validate_payload(payload)?;
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
                .headers(codex_oauth_headers(&self.client_version));
            if let Some(identity) = request_identity {
                request = request
                    .header(CODEX_SESSION_ID_HEADER, &identity.session_id)
                    .header(CODEX_THREAD_ID_HEADER, &identity.thread_id)
                    .header(CODEX_WINDOW_ID_HEADER, &identity.window_id);
            }
            let response = send_request_for_streaming_response(
                request.json(payload),
                self.header_timeout,
                "Codex Responses request failed",
                &url,
                request_context,
            )
            .await?;

            if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
                let retry_after = response
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|value| value.to_str().ok())
                    .and_then(super::parse_retry_after_seconds);
                let body = read_response_text_with_idle_timeout(
                    response,
                    self.stream_idle_timeout,
                    "Codex Responses 429 body read failed",
                    &url,
                    request_context,
                )
                .await?;

                if rate_limit_attempt >= MAX_429_RETRIES {
                    return Err(miette!(
                        "Codex Responses returned HTTP 429 after {} retries: {}",
                        MAX_429_RETRIES,
                        truncate_for_error(&body)
                    ));
                }

                let delay = retry_after.map_or_else(
                    || default_rate_limit_backoff(rate_limit_attempt),
                    Duration::from_secs,
                );
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
                let body = read_response_text_with_idle_timeout(
                    response,
                    self.stream_idle_timeout,
                    "Codex Responses 5xx body read failed",
                    &url,
                    request_context,
                )
                .await?;

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

    async fn complete_json(
        &self,
        request: PromptRequest,
        options: ModelRequestOptions,
    ) -> Result<Value> {
        let output_schema = request.output_schema.clone();
        let budget = &options.budget;
        let request_context = summarize_prompt_request(&request, Some(budget));
        let request_identity = codex_request_identity(options.conversation_id.as_deref());
        let payload = build_prompt_responses_payload(
            self,
            &request,
            &output_schema,
            request_identity.as_ref(),
        );
        let response = self
            .post_responses_with_retry(&payload, &request_context, request_identity.as_ref())
            .await?;
        let status = response.status();
        if !status.is_success() {
            let url = self.url();
            let body = read_response_text_with_idle_timeout(
                response,
                self.stream_idle_timeout,
                "Codex Responses body read failed",
                &url,
                &request_context,
            )
            .await?;
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
            return Err(miette!(
                "Codex Responses returned HTTP {}: {}",
                status,
                truncate_for_error(&body)
            ));
        }

        let result = self
            .parse_responses_stream(
                options.progress.as_ref(),
                response,
                false,
                options.conversation_id.as_deref(),
            )
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

    async fn complete_agent_turn(
        &self,
        request: AgentTurnRequest,
        options: ModelRequestOptions,
    ) -> Result<AgentTurnStreamResult> {
        let budget = &options.budget;
        let request_context = summarize_agent_turn_request(&request, Some(budget));
        let mut strip_images = !self.supports_vision.load(Ordering::Relaxed);
        let request_identity = codex_request_identity(options.conversation_id.as_deref());
        loop {
            let payload = build_agent_responses_payload(
                self,
                &request,
                strip_images,
                request_identity.as_ref(),
            );
            let response = self
                .post_responses_with_retry(&payload, &request_context, request_identity.as_ref())
                .await?;
            let status = response.status();
            if status.is_success() {
                return self
                    .parse_responses_stream(
                        options.progress.as_ref(),
                        response,
                        true,
                        options.conversation_id.as_deref(),
                    )
                    .await;
            }
            let url = self.url();
            let body = read_response_text_with_idle_timeout(
                response,
                self.stream_idle_timeout,
                "Codex Responses body read failed",
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
                    "Codex Responses rejected reasoning.summary; retrying agent turn without reasoning summaries\n{}",
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
                    "Codex Responses rejected image input; retrying agent turn without images\n{}",
                    request_context.join("\n")
                );
                continue;
            }
            return Err(miette!(
                "Codex Responses returned HTTP {}: {}",
                status,
                truncate_for_error(&body)
            ));
        }
    }

    async fn parse_responses_stream(
        &self,
        progress: Option<&ModelProgressSink>,
        response: reqwest::Response,
        emit_progress: bool,
        actor: Option<&str>,
    ) -> Result<AgentTurnStreamResult> {
        let mut diagnostics = super::stream_diagnostics::StreamDiagnostics::new(
            &response,
            "codex-responses",
            &self.model,
            actor,
        );
        let result = super::responses_protocol::parse_stream(
            response,
            progress.filter(|_| emit_progress),
            self.stream_idle_timeout,
            &self.model,
            &self.url(),
            &mut diagnostics,
            |usage| self.record_last_usage(usage),
        )
        .await;
        diagnostics.finish(result)
    }

    fn token_usage_info(&self) -> TokenUsageInfo {
        self.token_usage
            .lock()
            .ok()
            .map(|info| info.clone())
            .unwrap_or_default()
    }

    fn record_last_usage(&self, usage: TokenUsage) {
        if let Ok(mut info) = self.token_usage.lock() {
            info.model_context_window = i64::try_from(self.context_window_tokens).ok();
            info.append_last_usage(usage);
        }
    }
}

fn codex_http_client_builder(timeout: Duration) -> reqwest::ClientBuilder {
    crate::http_client::builder()
        .timeout(timeout)
        .pool_idle_timeout(Duration::from_secs(CODEX_HTTP_POOL_IDLE_TIMEOUT_SECS))
        .pool_max_idle_per_host(CODEX_HTTP_POOL_MAX_IDLE_PER_HOST)
        .tcp_keepalive(Duration::from_secs(CODEX_HTTP_TCP_KEEPALIVE_SECS))
}

fn codex_responses_http_client_builder() -> reqwest::ClientBuilder {
    crate::http_client::builder()
        .connect_timeout(Duration::from_secs(15))
        .pool_idle_timeout(Duration::from_secs(CODEX_HTTP_POOL_IDLE_TIMEOUT_SECS))
        .pool_max_idle_per_host(CODEX_HTTP_POOL_MAX_IDLE_PER_HOST)
        .tcp_keepalive(Duration::from_secs(CODEX_HTTP_TCP_KEEPALIVE_SECS))
}

impl CodexClient {
    pub(crate) fn new(
        auth_file: PathBuf,
        base_url: Option<&str>,
        model_config: &ModelConfig,
    ) -> Self {
        let base_url = base_url.unwrap_or(CODEX_RESPONSES_BASE_URL);
        let inner = CodexResponsesClient::new(base_url, model_config);
        Self::from_inner(inner, Some(auth_file))
    }

    pub(crate) fn with_api_key(api_key: &str, base_url: &str, model_config: &ModelConfig) -> Self {
        let mut inner = CodexResponsesClient::new(base_url, model_config);
        inner.set_auth(api_key.to_owned(), reqwest::header::HeaderMap::new());
        inner.max_output_tokens = Some(model_config.max_completion_tokens());
        Self::from_inner(inner, None)
    }

    fn from_inner(inner: CodexResponsesClient, auth_file: Option<PathBuf>) -> Self {
        let auth_client = codex_http_client_builder(Duration::from_secs(15))
            .build()
            .expect("failed to build OpenAI Codex auth http client");
        let request_budget_limits = inner.request_budget_limits();
        let model = inner.model.clone();
        Self {
            auth_file,
            auth_client,
            cached: tokio::sync::Mutex::new(None),
            inner: tokio::sync::Mutex::new(inner),
            request_budget_limits,
            model,
        }
    }

    async fn ensure_auth(&self) -> Result<()> {
        let Some(auth_file) = self.auth_file.as_deref() else {
            return Ok(());
        };
        let access = codex_oauth_access_from_file_with_client(auth_file, &self.auth_client)
            .await
            .map_err(|err| {
                let context = format!(
                    "OpenAI Codex auth at {} is unavailable",
                    auth_file.display()
                );
                err.wrap_err(context)
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
        drop(inner);
        *self.cached.lock().await = Some(access);
        Ok(())
    }
}

#[async_trait]
impl ModelProvider for CodexClient {
    async fn complete_json(
        &self,
        request: PromptRequest,
        options: ModelRequestOptions,
    ) -> Result<serde_json::Value> {
        self.ensure_auth().await?;
        self.inner
            .lock()
            .await
            .complete_json(request, options)
            .await
    }

    async fn complete_agent_turn(
        &self,
        request: AgentTurnRequest,
        options: ModelRequestOptions,
    ) -> Result<AgentTurnStreamResult> {
        self.ensure_auth().await?;
        self.inner
            .lock()
            .await
            .complete_agent_turn(request, options)
            .await
    }

    fn request_budget_limits(&self) -> RequestBudgetLimits {
        self.request_budget_limits
    }

    fn token_usage_info(&self) -> TokenUsageInfo {
        self.inner
            .try_lock()
            .map(|inner| inner.token_usage_info())
            .unwrap_or_default()
    }

    fn model_name(&self) -> String {
        self.model.clone()
    }
}

fn build_prompt_responses_payload(
    client: &CodexResponsesClient,
    request: &PromptRequest,
    output_schema: &Value,
    request_identity: Option<&CodexRequestIdentity>,
) -> Value {
    let messages = request.all_messages();
    // Prompt requests are structured output calls; pass strip_images=false as
    // prompts don't carry user-uploaded image attachments.
    let (instructions, input) = history_messages_to_responses_parts(messages, false);
    let mut payload = base_responses_payload(
        client,
        &instructions,
        &input,
        &Vec::new(),
        request_identity,
        "prompt",
    );
    payload["text"] = json!({
        "format": {
            "type": "json_schema",
            "name": sanitize_text_format_name(&request.tool_name),
            "strict": crate::schema_utils::supports_strict_schema(output_schema),
            "schema": output_schema,
        }
    });
    payload
}

fn build_agent_responses_payload(
    client: &CodexResponsesClient,
    request: &AgentTurnRequest,
    strip_images: bool,
    request_identity: Option<&CodexRequestIdentity>,
) -> Value {
    let (instructions, input) = messages_to_responses_parts(&request.messages, strip_images);
    let tools = request
        .tools
        .iter()
        .cloned()
        .map(tool_to_responses_tool)
        .collect::<Vec<_>>();
    base_responses_payload(
        client,
        &instructions,
        &input,
        &tools,
        request_identity,
        "agent",
    )
}

fn base_responses_payload(
    client: &CodexResponsesClient,
    instructions: &str,
    input: &[Value],
    tools: &[Value],
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
        "include": ["reasoning.encrypted_content"],
        "client_metadata": {
            "x-codex-installation-id": client.installation_id,
        },
    });
    if let Some(limit) = client.max_output_tokens {
        payload["max_output_tokens"] = json!(limit);
    }
    if let Some(identity) = request_identity {
        payload["prompt_cache_key"] = json!(prompt_cache_key(identity, request_kind));
        payload["client_metadata"][CODEX_WINDOW_ID_HEADER] = json!(identity.window_id);
    }
    if let Some(reasoning) = reasoning_payload(
        client.thinking_budget.as_deref(),
        request_kind == "agent" && client.supports_reasoning_summary.load(Ordering::Relaxed),
    ) {
        payload["reasoning"] = reasoning;
    }
    payload
}

fn codex_request_identity(conversation_id: Option<&str>) -> Option<CodexRequestIdentity> {
    let conversation_id = conversation_id?;
    Some(CodexRequestIdentity {
        session_id: conversation_id.to_string(),
        thread_id: conversation_id.to_string(),
        window_id: format!("{conversation_id}:0"),
    })
}

fn prompt_cache_key(identity: &CodexRequestIdentity, request_kind: &str) -> String {
    format!("daat-locus:{}:{request_kind}", identity.session_id)
}

/// Return the Daat-owned filename for one imported external Codex auth file.
/// The hash is calculated from the source bytes before they are normalized, so
/// each imported snapshot has a stable private destination.
pub fn imported_codex_oauth_auth_file(source_bytes: &[u8]) -> PathBuf {
    let digest = hex::encode(Sha256::digest(source_bytes));
    daat_locus_paths_sync().codex_auth_file(&format!("codex-auth-{digest}.json"))
}

pub const fn codex_oauth_default_base_url() -> &'static str {
    CODEX_RESPONSES_BASE_URL
}

pub fn codex_oauth_client_version() -> String {
    std::env::var(CODEX_CLIENT_VERSION_OVERRIDE_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| CODEX_CLIENT_VERSION.to_string())
}

pub(crate) fn codex_oauth_headers(client_version: &str) -> reqwest::header::HeaderMap {
    use reqwest::header::{HeaderMap, HeaderValue, USER_AGENT};
    let mut headers = HeaderMap::new();
    headers.insert("originator", HeaderValue::from_static(CODEX_ORIGINATOR));
    if let Ok(version) = HeaderValue::from_str(client_version) {
        headers.insert("version", version);
    }
    if let Ok(user_agent) = HeaderValue::from_str(&codex_user_agent(client_version)) {
        headers.insert(USER_AGENT, user_agent);
    }
    headers
}

/// Builds the Codex CLI compatible `User-Agent` header value.
///
/// The Codex backend identifies the client by this header, so the
/// originator/version prefix must match the current Codex CLI shape.
fn codex_user_agent(client_version: &str) -> String {
    format!(
        "{CODEX_ORIGINATOR}/{client_version} ({}; {})",
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}

pub fn codex_cli_auth_file() -> PathBuf {
    env::var_os("CODEX_HOME")
        .map_or_else(
            || {
                env::home_dir()
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join(".codex")
            },
            PathBuf::from,
        )
        .join("auth.json")
}

pub async fn write_codex_oauth_tokens(auth_file: &Path, tokens: &CodexOAuthTokens) -> Result<()> {
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

pub async fn codex_oauth_access_from_file(auth_file: &Path) -> Result<CodexOAuthAccess> {
    let client = crate::http_client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|err| miette!("failed to build OpenAI Codex auth http client: {err}"))?;
    codex_oauth_access_from_file_with_client(auth_file, &client).await
}

/// Retain a private snapshot and a read-only source binding. Imported refresh
/// tokens belong to the source client; consuming them here would fork its session.
pub async fn import_codex_cli_oauth_file(auth_file: &Path) -> Result<PathBuf> {
    let auth_file = tokio::fs::canonicalize(auth_file)
        .await
        .map_err(|err| miette!("read Codex auth file {} failed: {err}", auth_file.display()))?;
    let bytes = tokio::fs::read(&auth_file)
        .await
        .map_err(|err| miette!("read Codex auth file {} failed: {err}", auth_file.display()))?;
    let tokens = parse_codex_cli_oauth_tokens(&bytes).map_err(|err| {
        miette!(
            "parse Codex auth file {} failed: {err}",
            auth_file.display()
        )
    })?;
    let destination = imported_codex_oauth_auth_file(&bytes);
    let stored = StoredCodexAuth {
        tokens,
        imported_from: Some(auth_file),
    };
    let bytes = serde_json::to_vec_pretty(&stored)
        .map_err(|err| miette!("serialize imported Codex auth failed: {err}"))?;
    write_bytes_atomic(destination.clone(), bytes, PersistenceFileMode::Private)
        .await
        .map_err(|err| miette!("write imported Codex auth failed: {err}"))?;
    Ok(destination)
}

fn parse_codex_cli_oauth_tokens(bytes: &[u8]) -> Result<CodexOAuthTokens> {
    let auth: CodexCliAuthFile = serde_json::from_slice(bytes).map_err(|err| {
        miette!(
            "expected Codex CLI auth.json with a tokens object containing id_token, access_token, and refresh_token: {err}"
        )
    })?;
    let tokens = auth.tokens.ok_or_else(|| {
        let has_api_key = auth
            .openai_api_key
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty());
        if has_api_key {
            miette!(
                "this Codex auth.json uses OPENAI_API_KEY authentication and cannot be imported as Codex OAuth; configure it as an OpenAI API key provider instead"
            )
        } else {
            miette!(
                "expected Codex CLI auth.json with a tokens object containing id_token, access_token, and refresh_token"
            )
        }
    })?;
    Ok(CodexOAuthTokens {
        id_token: tokens.id_token,
        access_token: tokens.access_token,
        refresh_token: tokens.refresh_token,
        account_id: tokens.account_id,
        last_refresh_at_ms: now_ms(),
    })
}

async fn codex_oauth_access_from_file_with_client(
    auth_file: &Path,
    client: &reqwest::Client,
) -> Result<CodexOAuthAccess> {
    let endpoint = std::env::var(CODEX_OAUTH_REFRESH_URL_OVERRIDE_ENV)
        .unwrap_or_else(|_| CODEX_OAUTH_REFRESH_URL.to_string());
    codex_oauth_access_with_refresh_endpoint(auth_file, client, &endpoint).await
}

async fn codex_oauth_access_with_refresh_endpoint(
    auth_file: &Path,
    client: &reqwest::Client,
    endpoint: &str,
) -> Result<CodexOAuthAccess> {
    let stored = read_codex_auth(auth_file).await?;
    if let Some(source) = stored.imported_from.as_deref() {
        return imported_codex_access(&stored.tokens, source).await;
    }
    let mut tokens = stored.tokens;
    if codex_oauth_tokens_need_refresh(&tokens) {
        let refresh_lock = refresh_lock_for_auth_file(auth_file);
        let _guard = refresh_lock.lock().await;
        let stored = read_codex_auth(auth_file).await?;
        if let Some(source) = stored.imported_from.as_deref() {
            return imported_codex_access(&stored.tokens, source).await;
        }
        tokens = stored.tokens;
        if !codex_oauth_tokens_need_refresh(&tokens) {
            return Ok(codex_oauth_access_from_tokens(&tokens));
        }
        tokens = refresh_codex_oauth_tokens(client, &tokens, endpoint).await?;
        write_codex_oauth_tokens(auth_file, &tokens).await?;
    }
    Ok(codex_oauth_access_from_tokens(&tokens))
}

fn refresh_lock_for_auth_file(auth_file: &Path) -> Arc<tokio::sync::Mutex<()>> {
    let mut locks = REFRESH_LOCKS_BY_AUTH_FILE.lock();
    locks
        .entry(auth_file.to_path_buf())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

async fn read_codex_auth(auth_file: &Path) -> Result<StoredCodexAuth> {
    let bytes = tokio::fs::read(auth_file)
        .await
        .map_err(|err| miette!("read {} failed: {err}", auth_file.display()))?;
    serde_json::from_slice(&bytes)
        .map_err(|err| miette!("parse {} failed: {err}", auth_file.display()))
}

fn codex_authentication_error(detail: String) -> miette::Report {
    super::ModelRequestFailure::report(super::ModelFailureKind::Authentication, detail)
}

async fn imported_codex_access(
    snapshot: &CodexOAuthTokens,
    source: &Path,
) -> Result<CodexOAuthAccess> {
    let tokens = match tokio::fs::read(source).await {
        Ok(bytes) => {
            let current = parse_codex_cli_oauth_tokens(&bytes).map_err(|_| {
                codex_authentication_error(
                    "Imported Codex source no longer contains a usable login; sign in with Codex or log in separately in Daat Locus".into(),
                )
            })?;
            if !same_codex_identity(snapshot, &current) {
                return Err(codex_authentication_error(
                    "Imported Codex source changed account; explicitly import it again or log in separately in Daat Locus".into(),
                ));
            }
            current
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => snapshot.clone(),
        Err(_) => {
            return Err(codex_authentication_error(
                "Imported Codex source cannot be read; restore access or log in separately in Daat Locus".into(),
            ));
        }
    };
    if codex_oauth_tokens_need_refresh(&tokens) {
        return Err(codex_authentication_error(
            "Imported Codex access token expired; let Codex renew its login, import a current auth file, or log in separately in Daat Locus. Imported refresh tokens are never consumed by Daat Locus".into(),
        ));
    }
    Ok(codex_oauth_access_from_tokens(&tokens))
}

fn same_codex_identity(left: &CodexOAuthTokens, right: &CodexOAuthTokens) -> bool {
    let subject = |tokens: &CodexOAuthTokens| {
        decode_jwt_payload::<JwtPayload>(&tokens.id_token)
            .ok()
            .and_then(|claims| claims.sub)
            .filter(|sub| !sub.is_empty())
    };
    let same_subject = match (subject(left), subject(right)) {
        (Some(left), Some(right)) => left == right,
        _ => left.id_token == right.id_token,
    };
    same_subject
        && codex_oauth_access_from_tokens(left).account_id
            == codex_oauth_access_from_tokens(right).account_id
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
    endpoint: &str,
) -> Result<CodexOAuthTokens> {
    let response = client
        .post(endpoint)
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
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::BAD_REQUEST
        {
            return Err(codex_authentication_error(format!(
                "OpenAI Codex token refresh returned HTTP {status}: {body}. Sign in again in Daat Locus; for an old imported snapshot, import the current source again"
            )));
        }
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

pub fn codex_oauth_access_from_tokens(tokens: &CodexOAuthTokens) -> CodexOAuthAccess {
    let claims = jwt_openai_auth_claims(&tokens.id_token);
    CodexOAuthAccess {
        access_token: tokens.access_token.clone(),
        account_id: tokens.account_id.clone().or_else(|| {
            claims
                .as_ref()
                .and_then(|claims| claims.chatgpt_account_id.clone())
        }),
        is_fedramp_account: claims.is_some_and(|claims| claims.chatgpt_account_is_fedramp),
        expires_at_ms: access_token_expires_at_ms(&tokens.access_token),
    }
}

fn access_token_expires_at_ms(access_token: &str) -> Option<i64> {
    let payload = decode_jwt_payload::<JwtPayload>(access_token).ok()?;
    payload.exp.map(|exp_secs| exp_secs.saturating_mul(1000))
}

fn jwt_openai_auth_claims(jwt: &str) -> Option<OpenAiAuthClaims> {
    decode_jwt_payload::<JwtPayload>(jwt).ok()?.auth
}

/// Unsigned JWT payload decoder. Signature verification is intentionally off:
/// this path only reads claims from tokens we already hold.
fn decode_jwt_payload<T: DeserializeOwned>(jwt: &str) -> Result<T> {
    let payload = jwt_payload_segment(jwt)?;
    let bytes = decode_jwt_base64url_segment(payload)?;
    serde_json::from_slice(&bytes).map_err(|err| miette!("invalid JWT JSON payload: {err}"))
}

/// A JWT is exactly three non-empty dot-separated segments. Extra segments
/// (a fourth `.`) are rejected rather than ignored.
fn jwt_payload_segment(jwt: &str) -> Result<&str> {
    let mut segments = jwt.split('.');
    let header = segments.next();
    let payload = segments.next();
    let signature = segments.next();
    let extra = segments.next();
    match (header, payload, signature, extra) {
        (Some(header), Some(payload), Some(signature), None)
            if !header.is_empty() && !payload.is_empty() && !signature.is_empty() =>
        {
            Ok(payload)
        }
        _ => Err(miette!("invalid JWT format")),
    }
}

fn decode_jwt_base64url_segment(segment: &str) -> Result<Vec<u8>> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(segment)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(segment))
        .map_err(|err| miette!("invalid JWT base64 payload: {err}"))
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
#[path = "codex_oauth/auth_tests.rs"]
mod auth_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model_protocol::AgentMessage;

    fn thinking_budget(value: &str) -> ThinkingBudget {
        serde_json::from_value(serde_json::json!(value)).expect("thinking budget deserializes")
    }
    use crate::config::{ModelConfig, ThinkingBudget};

    pub(super) fn jwt(payload: &serde_json::Value) -> String {
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
    fn parse_codex_cli_auth_file_rejects_api_key_authentication() {
        let error = parse_codex_cli_oauth_tokens(
            br#"{
                "auth_mode": "api_key",
                "OPENAI_API_KEY": "sk-example"
            }"#,
        )
        .expect_err("API key auth must not be imported as OAuth");

        assert!(error.to_string().contains("OPENAI_API_KEY authentication"));
    }

    #[test]
    fn parse_codex_cli_auth_file_requires_tokens() {
        let error = parse_codex_cli_oauth_tokens(br#"{"auth_mode":"chatgpt"}"#)
            .expect_err("missing tokens must be rejected");

        assert!(error.to_string().contains("tokens object"));
    }
    #[test]
    fn decode_jwt_payload_rejects_four_segments() {
        let token = jwt(&serde_json::json!({ "sub": "user" }));
        let four_segment = format!("{token}.extra");

        let error = decode_jwt_payload::<serde_json::Value>(&four_segment)
            .expect_err("a fourth JWT segment must be rejected");

        assert!(error.to_string().contains("invalid JWT format"));
    }

    #[test]
    fn decode_jwt_payload_reads_unsigned_three_segment_claims() {
        let token = jwt(&serde_json::json!({
            "sub": "user-1",
            "exp": 4_102_444_800_i64
        }));

        let payload =
            decode_jwt_payload::<serde_json::Value>(&token).expect("unsigned JWT payload");

        assert_eq!(payload["sub"], "user-1");
        assert_eq!(payload["exp"], 4_102_444_800_i64);
        assert_eq!(token.matches('.').count(), 3 - 1);
    }

    #[test]
    fn codex_oauth_access_extracts_account_headers_from_id_token() {
        let id_token = jwt(&serde_json::json!({
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "account-123",
                "chatgpt_account_is_fedramp": true
            }
        }));
        let access_token = jwt(&serde_json::json!({
            "exp": 4_102_444_800_i64
        }));
        let tokens = CodexOAuthTokens {
            id_token,
            access_token: access_token.clone(),
            refresh_token: "refresh".to_string(),
            account_id: None,
            last_refresh_at_ms: 0,
        };

        let access = codex_oauth_access_from_tokens(&tokens);

        assert_eq!(access.access_token, access_token);
        assert_eq!(access.account_id.as_deref(), Some("account-123"));
        assert!(access.is_fedramp_account);
        assert_eq!(access.expires_at_ms, Some(4_102_444_800_000));
    }

    #[test]
    fn codex_oauth_configured_account_id_overrides_id_token_claim() {
        let id_token = jwt(&serde_json::json!({
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

        let access = codex_oauth_access_from_tokens(&tokens);

        assert_eq!(access.account_id.as_deref(), Some("configured-account"));
    }

    #[tokio::test]
    async fn import_codex_cli_auth_file_rejects_a_directory() {
        let temp = tempfile::tempdir().expect("tempdir");
        let error = import_codex_cli_oauth_file(temp.path())
            .await
            .expect_err("directories are not valid Codex auth files");

        assert!(error.to_string().contains("read Codex auth file"));
    }

    #[tokio::test]
    async fn import_codex_cli_auth_file_reports_malformed_json_without_tokens() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("auth.json");
        tokio::fs::write(&source, b"not-json")
            .await
            .expect("write malformed source");

        let error = import_codex_cli_oauth_file(&source)
            .await
            .expect_err("malformed auth file must fail");

        let message = error.to_string();
        assert!(message.contains("parse Codex auth file"));
        assert!(message.contains("tokens object"));
        assert!(!message.contains("not-json"));
    }

    #[tokio::test]
    async fn provider_import_remains_available_after_external_source_is_deleted() {
        let temp = tempfile::tempdir().expect("tempdir");
        let _home_override =
            crate::daat_locus_paths::DaatLocusHomeOverride::set(temp.path().to_path_buf()).await;
        let source = temp.path().join("external").join("auth.json");
        tokio::fs::create_dir_all(source.parent().expect("source parent"))
            .await
            .expect("create source parent");
        tokio::fs::write(
            &source,
            br#"{
                "tokens": {
                    "id_token": "id-token",
                    "access_token": "access-token",
                    "refresh_token": "refresh-token"
                }
            }"#,
        )
        .await
        .expect("write external Codex auth");

        let source_bytes = tokio::fs::read(&source)
            .await
            .expect("read external Codex auth");
        let destination = import_codex_cli_oauth_file(&source)
            .await
            .expect("import external Codex auth");
        tokio::fs::remove_file(&source)
            .await
            .expect("delete external Codex auth");

        let access = codex_oauth_access_from_file(&destination)
            .await
            .expect("read imported Daat auth after source deletion");
        assert_eq!(access.access_token, "access-token");
        assert_eq!(destination, imported_codex_oauth_auth_file(&source_bytes));
    }

    #[tokio::test]
    async fn independently_owned_auth_refreshes_and_persists_rotated_tokens() {
        let temp = tempfile::tempdir().expect("tempdir");
        let _home_override =
            crate::daat_locus_paths::DaatLocusHomeOverride::set(temp.path().to_path_buf()).await;
        let destination = temp.path().join("owned.json");

        let expired_access_token = jwt(&serde_json::json!({"exp": 1}));
        write_codex_oauth_tokens(
            &destination,
            &CodexOAuthTokens {
                id_token: "id-token".to_string(),
                access_token: expired_access_token,
                refresh_token: "refresh-token".to_string(),
                account_id: None,
                last_refresh_at_ms: 0,
            },
        )
        .await
        .expect("write expired imported auth");

        let server = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind refresh server");
        let endpoint = format!(
            "http://{}/oauth/token",
            server.local_addr().expect("server addr")
        );
        let refresh_body = r#"{"access_token":"refreshed-access-token","refresh_token":"refreshed-refresh-token"}"#;
        let refresh_response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            refresh_body.len(),
            refresh_body
        );
        let server_task = tokio::spawn(async move {
            let (mut stream, _) = server.accept().await.expect("accept refresh request");
            use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
            let mut request = vec![0_u8; 4096];
            let _ = stream
                .read(&mut request)
                .await
                .expect("read refresh request");
            stream
                .write_all(refresh_response.as_bytes())
                .await
                .expect("write refresh response");
        });
        // The disposable loopback server must never inherit a machine proxy.
        let client = crate::http_client::builder().no_proxy().build().unwrap();
        let accesses =
            futures_util::future::try_join_all((0..8).map(|_| {
                codex_oauth_access_with_refresh_endpoint(&destination, &client, &endpoint)
            }))
            .await
            .expect("concurrent clients must share one persisted token rotation");
        server_task.await.expect("refresh server task");

        assert!(
            accesses
                .iter()
                .all(|access| access.access_token == "refreshed-access-token")
        );
        let refreshed: CodexOAuthTokens = serde_json::from_slice(
            &tokio::fs::read(&destination)
                .await
                .expect("read refreshed Daat auth"),
        )
        .expect("parse refreshed Daat auth");
        assert_eq!(refreshed.access_token, "refreshed-access-token");
        assert_eq!(refreshed.refresh_token, "refreshed-refresh-token");
        let access = codex_oauth_access_with_refresh_endpoint(&destination, &client, &endpoint)
            .await
            .expect("persisted token must not refresh again against the stopped server");
        assert_eq!(access.access_token, "refreshed-access-token");
    }

    #[tokio::test]
    async fn import_codex_cli_auth_file_writes_only_flat_daat_tokens() {
        let temp = tempfile::tempdir().expect("tempdir");
        let _home_override =
            crate::daat_locus_paths::DaatLocusHomeOverride::set(temp.path().to_path_buf()).await;
        let source = temp.path().join("external").join("auth.json");
        tokio::fs::create_dir_all(source.parent().expect("source parent"))
            .await
            .expect("create source parent");
        tokio::fs::write(
            &source,
            br#"{
                "auth_mode": "chatgpt",
                "tokens": {
                    "id_token": "id-token",
                    "access_token": "access-token",
                    "refresh_token": "refresh-token"
                }
            }"#,
        )
        .await
        .expect("write external Codex auth");

        let destination = import_codex_cli_oauth_file(&source)
            .await
            .expect("import external Codex auth");
        let value: Value = serde_json::from_slice(
            &tokio::fs::read(&destination)
                .await
                .expect("read imported Daat auth"),
        )
        .expect("parse imported Daat auth");

        assert!(destination.starts_with(temp.path().join("codex-auth")));
        assert!(
            destination
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("codex-auth-"))
        );
        assert_eq!(value["id_token"], "id-token");
        assert_eq!(value["access_token"], "access-token");
        assert_eq!(value["refresh_token"], "refresh-token");
        assert!(value.get("tokens").is_none());
        assert!(value.get("auth_mode").is_none());
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
    fn codex_payload_omits_unsupported_max_output_tokens_parameter() {
        let payload =
            base_responses_payload(&test_client(), "instructions", &[], &[], None, "agent");

        assert!(payload.get("max_output_tokens").is_none(), "{payload:#}");
    }

    #[test]
    fn codex_agent_payload_requests_reasoning_summary() {
        let payload = build_agent_responses_payload(
            &test_client(),
            &AgentTurnRequest {
                messages: vec![AgentMessage::system("base"), AgentMessage::user("work")],
                tools: Vec::new(),
            },
            false,
            None,
        );

        assert_eq!(payload["reasoning"]["summary"], "auto");
        assert_eq!(payload["include"], json!(["reasoning.encrypted_content"]));
        assert!(payload["reasoning"].get("effort").is_none(), "{payload:#}");
    }

    #[test]
    fn codex_prompt_payload_does_not_request_reasoning_summary() {
        let payload =
            base_responses_payload(&test_client(), "instructions", &[], &[], None, "prompt");

        assert!(payload.get("reasoning").is_none(), "{payload:#}");
    }

    #[test]
    fn codex_reasoning_summary_can_be_disabled_without_dropping_effort() {
        let client = CodexResponsesClient::new(
            CODEX_RESPONSES_BASE_URL,
            &ModelConfig {
                model_id: "gpt-5.4".to_string(),
                provider: "codex-oauth".to_string(),
                thinking_budget: Some(thinking_budget("max")),
                ..ModelConfig::default()
            },
        );
        client
            .supports_reasoning_summary
            .store(false, Ordering::Relaxed);

        let payload = base_responses_payload(&client, "instructions", &[], &[], None, "agent");

        assert_eq!(payload["reasoning"]["effort"], "max");
        assert!(payload["reasoning"].get("summary").is_none(), "{payload:#}");
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

        let first = build_agent_responses_payload(&client, &request, false, Some(&identity));
        let second = build_agent_responses_payload(&client, &request, false, Some(&identity));

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

        let payload = build_agent_responses_payload(&client, &request, false, None);

        assert!(payload.get("prompt_cache_key").is_none());
    }
}
