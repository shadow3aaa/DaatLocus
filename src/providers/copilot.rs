use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use miette::{Result, miette};

use crate::{
    config::{ModelConfig, redact_secret_text},
    core::{ModelProvider, ModelRequestOptions, TokenUsageInfo},
    reasoning::runtime::{AgentTurnRequest, AgentTurnStreamResult, PromptRequest},
};

use super::OpenAIClient;

// ---------------------------------------------------------------------------
// CopilotClient prefers a session token for the internal full-model API and
// falls back to the public API when needed.
// ---------------------------------------------------------------------------

const COPILOT_USER_AGENT: &str = "GitHubCopilotChat/0.26.7";
const COPILOT_EDITOR_VERSION: &str = "vscode/1.96.2";
const COPILOT_GITHUB_API_VERSION: &str = "2025-04-01";
const COPILOT_INTERNAL_BASE_URL: &str = "https://api.individual.githubcopilot.com";

struct CopilotSessionToken {
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
        let inner =
            OpenAIClient::from_parts("placeholder", COPILOT_INTERNAL_BASE_URL, model_config);
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
            cached
                .as_ref()
                .is_none_or(|t| now_secs + 60 >= t.expires_at_secs)
        };

        if !needs_exchange {
            return Ok(());
        }

        let (token, base_url, expires_at_secs) = self.exchange_session_token().await?;
        tracing::info!(base_url = %base_url, "copilot: session token acquired");

        let mut hdrs = reqwest::header::HeaderMap::new();
        hdrs.insert("Editor-Version", COPILOT_EDITOR_VERSION.parse().unwrap());
        hdrs.insert("User-Agent", COPILOT_USER_AGENT.parse().unwrap());
        hdrs.insert(
            "X-Github-Api-Version",
            COPILOT_GITHUB_API_VERSION.parse().unwrap(),
        );

        let mut inner = self.inner.lock().await;
        inner.api_key.clone_from(&token);
        inner.base_url.clone_from(&base_url);
        inner.completions_path = "/chat/completions";
        inner.extra_headers = hdrs;
        drop(inner);

        *self.cached.lock().await = Some(CopilotSessionToken { expires_at_secs });
        Ok(())
    }

    async fn exchange_session_token(&self) -> Result<(String, String, u64)> {
        exchange_copilot_session_token_with_client(&self.auth_client, &self.github_token).await
    }
}

async fn exchange_copilot_session_token_with_client(
    auth_client: &reqwest::Client,
    github_token: &str,
) -> Result<(String, String, u64)> {
    tracing::debug!("copilot: exchanging github token for session token");
    let resp = auth_client
        .get("https://api.github.com/copilot_internal/v2/token")
        .header("Authorization", format!("Bearer {github_token}"))
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
        let body = redact_secret_text(&body, github_token);
        tracing::debug!(http_status = %status, body = %body, "copilot session token exchange non-2xx");
        return Err(miette!("HTTP {status}"));
    }

    let json: serde_json::Value = resp.json().await.map_err(|e| miette!("parse error: {e}"))?;

    let token = json["token"]
        .as_str()
        .ok_or_else(|| miette!("missing 'token' field"))?
        .to_string();
    let expires_at_secs = json["expires_at"].as_u64().unwrap_or(0);
    let base_url = derive_copilot_base_url(&token);

    Ok((token, base_url, expires_at_secs))
}

/// Session token is a semicolon-separated key=value string; derive API base URL from proxy-ep.
fn derive_copilot_base_url(session_token: &str) -> String {
    session_token
        .split(';')
        .find_map(|part| {
            let trimmed = part.trim();
            let val = trimmed.to_lowercase();
            val.strip_prefix("proxy-ep=").and_then(|_| {
                let host = &trimmed[9..];
                if host.is_empty() {
                    return None;
                }
                let host = if host.to_lowercase().starts_with("proxy.") {
                    format!("api.{}", &host[6..])
                } else {
                    host.to_string()
                };
                Some(format!("https://{host}"))
            })
        })
        .unwrap_or_else(|| COPILOT_INTERNAL_BASE_URL.to_string())
}

#[async_trait]
impl ModelProvider for CopilotClient {
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

    fn request_budget_limits(&self) -> crate::context_budget::RequestBudgetLimits {
        self.inner.try_lock().map_or(
            crate::context_budget::RequestBudgetLimits {
                context_window_tokens: crate::context_budget::DEFAULT_CONTEXT_WINDOW_TOKENS,
                auto_compact_threshold_tokens: crate::context_budget::DEFAULT_CONTEXT_WINDOW_TOKENS,
                reserved_output_tokens: crate::context_budget::DEFAULT_MAX_COMPLETION_TOKENS,
            },
            |inner| inner.request_budget_limits(),
        )
    }

    fn token_usage_info(&self) -> TokenUsageInfo {
        self.inner
            .try_lock()
            .map(|inner| inner.token_usage_info())
            .unwrap_or_default()
    }

    fn model_name(&self) -> String {
        self.inner
            .try_lock()
            .map(|inner| inner.model_name())
            .unwrap_or_default()
    }
}
