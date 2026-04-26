//! Local OpenAI-compatible LLM proxy for hindsight-embed.
//!
//! hindsight-embed only knows how to call OpenAI-shaped HTTP APIs. Daat Locus
//! providers are broader than that, so daemon startup points hindsight at this
//! local proxy and the proxy delegates to the configured hindsight model.

use std::{
    net::{Ipv4Addr, SocketAddr},
    sync::{Arc, Mutex as StdMutex},
};

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use miette::{Result, miette};
use serde_json::{Value, json};
use tokio::{net::TcpListener, sync::oneshot};
use uuid::Uuid;

use crate::{
    config::{Config, ModelConfig, ProviderConfig, resolve_env_reference},
    providers::{CodexOAuthClient, CopilotClient, OpenAIClient},
};

#[derive(Clone)]
pub(crate) struct HindsightLlmProxy {
    inner: Arc<HindsightLlmProxyInner>,
}

struct HindsightLlmProxyInner {
    base_url: String,
    api_key: String,
    model_id: String,
    shutdown_tx: StdMutex<Option<oneshot::Sender<()>>>,
}

impl Drop for HindsightLlmProxyInner {
    fn drop(&mut self) {
        if let Ok(mut shutdown_tx) = self.shutdown_tx.lock()
            && let Some(shutdown_tx) = shutdown_tx.take()
        {
            let _ = shutdown_tx.send(());
        }
    }
}

#[derive(Clone)]
struct ProxyState {
    api_key: Arc<str>,
    model_id: Arc<str>,
    target: Arc<HindsightLlmProxyTarget>,
}

enum HindsightLlmProxyTarget {
    OpenAi(OpenAIClient),
    Copilot(CopilotClient),
    CodexOAuth(CodexOAuthClient),
}

impl HindsightLlmProxy {
    pub(crate) async fn start(config: &Config) -> Result<Self> {
        let model = config.hindsight_model_config().clone();
        let provider = config.hindsight_provider_config().clone();
        let target = HindsightLlmProxyTarget::from_config(&model, &provider)?;
        let api_key = format!("daat-locus-hindsight-proxy-{}", Uuid::new_v4());

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|err| miette!("failed to bind hindsight LLM proxy: {err}"))?;
        let addr = listener
            .local_addr()
            .map_err(|err| miette!("failed to read hindsight LLM proxy address: {err}"))?;
        let base_url = format!("http://{}/v1", format_socket_addr(addr));

        let state = ProxyState {
            api_key: Arc::from(api_key.as_str()),
            model_id: Arc::from(model.model_id.as_str()),
            target: Arc::new(target),
        };
        let router = Router::new()
            .route("/v1/chat/completions", post(handle_chat_completions))
            .route("/chat/completions", post(handle_chat_completions))
            .route("/v1/models", get(handle_models))
            .route("/models", get(handle_models))
            .with_state(state);
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        tokio::spawn(async move {
            let server = axum::serve(listener, router).with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
            });
            if let Err(err) = server.await {
                tracing::warn!("[hindsight:llm-proxy] server exited with error: {err}");
            }
        });

        tracing::info!(
            "[hindsight:llm-proxy] listening at {} for model {}",
            base_url,
            model.model_id,
        );
        Ok(Self {
            inner: Arc::new(HindsightLlmProxyInner {
                base_url,
                api_key,
                model_id: model.model_id,
                shutdown_tx: StdMutex::new(Some(shutdown_tx)),
            }),
        })
    }

    pub(crate) fn env_vars(&self) -> Vec<(String, String)> {
        vec![
            ("HINDSIGHT_API_LLM_PROVIDER".into(), "openai".into()),
            (
                "HINDSIGHT_API_LLM_API_KEY".into(),
                self.inner.api_key.clone(),
            ),
            (
                "HINDSIGHT_API_LLM_MODEL".into(),
                self.inner.model_id.clone(),
            ),
            (
                "HINDSIGHT_API_LLM_BASE_URL".into(),
                self.inner.base_url.clone(),
            ),
        ]
    }
}

impl HindsightLlmProxyTarget {
    fn from_config(model: &ModelConfig, provider: &ProviderConfig) -> Result<Self> {
        match provider {
            ProviderConfig::Openai { api_key, base_url } => {
                let base_url = base_url.as_deref().unwrap_or("https://api.openai.com/v1");
                Ok(Self::OpenAi(OpenAIClient::from_parts(
                    &resolve_env_reference(api_key),
                    &resolve_env_reference(base_url),
                    model,
                )))
            }
            ProviderConfig::OpenaiCompatible { base_url, api_key } => {
                Ok(Self::OpenAi(OpenAIClient::from_parts(
                    &resolve_env_reference(api_key),
                    &resolve_env_reference(base_url),
                    model,
                )))
            }
            ProviderConfig::GithubCopilot { github_token } => Ok(Self::Copilot(
                CopilotClient::new(&resolve_env_reference(github_token), model),
            )),
            ProviderConfig::OpenaiCodexOauth {
                auth_file,
                base_url,
            } => Ok(Self::CodexOAuth(CodexOAuthClient::new(
                &model.provider,
                auth_file.as_deref(),
                base_url.as_deref(),
                model,
            ))),
        }
    }

    async fn chat_completion(&self, payload: Value) -> Result<Value> {
        match self {
            Self::OpenAi(client) => client.post_compatible_chat_completion(payload).await,
            Self::Copilot(client) => client.post_compatible_chat_completion(payload).await,
            Self::CodexOAuth(client) => client.post_compatible_chat_completion(payload).await,
        }
    }
}

async fn handle_chat_completions(
    State(state): State<ProxyState>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Response {
    if !authorized(&headers, state.api_key.as_ref()) {
        return proxy_error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "invalid bearer token",
        );
    }
    if payload
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return proxy_error(
            StatusCode::BAD_REQUEST,
            "unsupported_request",
            "hindsight LLM proxy does not support streaming chat completions",
        );
    }

    match state.target.chat_completion(payload).await {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(err) => {
            tracing::warn!("[hindsight:llm-proxy] chat completion failed: {err:?}");
            proxy_error(StatusCode::BAD_GATEWAY, "upstream_error", &err.to_string())
        }
    }
}

async fn handle_models(State(state): State<ProxyState>, headers: HeaderMap) -> Response {
    if !authorized(&headers, state.api_key.as_ref()) {
        return proxy_error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "invalid bearer token",
        );
    }
    (
        StatusCode::OK,
        Json(json!({
            "object": "list",
            "data": [{
                "id": state.model_id.as_ref(),
                "object": "model",
                "created": 0,
                "owned_by": "daat-locus",
            }],
        })),
    )
        .into_response()
}

fn authorized(headers: &HeaderMap, api_key: &str) -> bool {
    let Some(value) = headers.get(header::AUTHORIZATION) else {
        return false;
    };
    let Ok(value) = value.to_str() else {
        return false;
    };
    value
        .strip_prefix("Bearer ")
        .is_some_and(|token| token == api_key)
}

fn proxy_error(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(json!({
            "error": {
                "message": message,
                "type": "daat_locus_hindsight_proxy_error",
                "code": code,
            }
        })),
    )
        .into_response()
}

fn format_socket_addr(addr: SocketAddr) -> String {
    match addr {
        SocketAddr::V4(addr) => addr.to_string(),
        SocketAddr::V6(addr) => format!("[{}]:{}", addr.ip(), addr.port()),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[tokio::test]
    async fn proxy_env_vars_use_local_openai_compatible_endpoint() {
        let config = Config::default();
        let proxy = HindsightLlmProxy::start(&config).await.unwrap();
        let vars: HashMap<_, _> = proxy.env_vars().into_iter().collect();

        assert_eq!(
            vars.get("HINDSIGHT_API_LLM_PROVIDER").map(String::as_str),
            Some("openai")
        );
        assert_eq!(
            vars.get("HINDSIGHT_API_LLM_MODEL").map(String::as_str),
            Some("gpt-4.1")
        );
        assert!(
            vars.get("HINDSIGHT_API_LLM_BASE_URL")
                .is_some_and(|url| url.starts_with("http://127.0.0.1:") && url.ends_with("/v1"))
        );
        assert!(!vars.contains_key("HINDSIGHT_API_SKIP_LLM_VERIFICATION"));
    }
}
