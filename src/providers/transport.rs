//! One HTTP boundary for model protocols. Callers supply payloads and model
//! context; authentication, gateway identity and retry bookkeeping live here.
use super::{
    default_rate_limit_backoff, opencode_gateway_headers, parse_retry_after_seconds,
    read_response_text_with_timeout, send_request_for_streaming_response,
    shared_request_rate_limiter, truncate_for_error,
};
use crate::{config::ModelConfig, core::ModelRequestOptions};
use miette::{Result, miette};
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use std::{
    collections::VecDeque,
    sync::Arc,
    time::{Duration, Instant},
};

pub(super) enum Authentication<'a> {
    Bearer(&'a str),
    Anthropic(&'a str),
}

pub(super) struct BackendHttp {
    client: reqwest::Client,
    base_url: String,
    rpm: Option<usize>,
    limiter: Option<Arc<tokio::sync::Mutex<VecDeque<Instant>>>>,
}

pub(super) struct BackendRequest<'a> {
    http: &'a BackendHttp,
    url: String,
    headers: HeaderMap,
}

impl BackendHttp {
    pub(super) fn new(base_url: &str, model: &ModelConfig) -> Self {
        Self {
            client: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(15))
                .build()
                .expect("failed to build model HTTP client"),
            base_url: base_url.to_owned(),
            rpm: model.rpm(),
            limiter: shared_request_rate_limiter(base_url, &model.model_id, model.rpm()),
        }
    }

    pub(super) fn request(
        &self,
        path: &str,
        authentication: Authentication<'_>,
        extra: &HeaderMap,
        options: &ModelRequestOptions,
    ) -> Result<BackendRequest<'_>> {
        let mut headers = extra.clone();
        match authentication {
            Authentication::Bearer(key) | Authentication::Anthropic(key) => {
                let mut value = HeaderValue::from_str(&format!("Bearer {key}"))
                    .map_err(|_| miette!("Invalid provider authorization"))?;
                value.set_sensitive(true);
                headers.insert(AUTHORIZATION, value);
            }
        }
        if let Authentication::Anthropic(key) = authentication {
            let mut value = HeaderValue::from_str(key)
                .map_err(|_| miette!("Invalid provider authorization"))?;
            value.set_sensitive(true);
            headers.insert("x-api-key", value);
            headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
        }
        headers.extend(opencode_gateway_headers(
            &self.base_url,
            options.conversation_id.as_deref(),
        ));
        Ok(BackendRequest {
            http: self,
            url: format!("{}{}", self.base_url.trim_end_matches('/'), path),
            headers,
        })
    }

    async fn wait(&self) {
        let (Some(rpm), Some(limiter)) = (self.rpm, self.limiter.as_ref()) else {
            return;
        };
        loop {
            let wait = {
                let mut queue = limiter.lock().await;
                let now = Instant::now();
                while queue
                    .front()
                    .is_some_and(|old| now.duration_since(*old) >= Duration::from_secs(60))
                {
                    queue.pop_front();
                }
                if queue.len() < rpm {
                    queue.push_back(now);
                    None
                } else {
                    queue
                        .front()
                        .map(|old| Duration::from_secs(60).saturating_sub(now.duration_since(*old)))
                }
            };
            match wait {
                None => return,
                Some(delay) => tokio::time::sleep(delay).await,
            }
        }
    }
}

impl BackendRequest<'_> {
    pub(super) async fn post(
        &self,
        payload: &serde_json::Value,
        diagnostics: &[String],
        header_timeout: Duration,
    ) -> Result<reqwest::Response> {
        let (mut rate_retries, mut server_retries) = (0usize, 0usize);
        loop {
            self.http.wait().await;
            let response = send_request_for_streaming_response(
                self.http
                    .client
                    .post(&self.url)
                    .headers(self.headers.clone())
                    .json(payload),
                header_timeout,
                "Model request failed",
                &self.url,
                diagnostics,
            )
            .await?;
            let status = response.status();
            if matches!(
                status,
                reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
            ) {
                let body = read_response_text_with_timeout(
                    response,
                    header_timeout,
                    "Model error response read failed",
                    &self.url,
                    diagnostics,
                )
                .await?;
                return Err(super::ModelRequestFailure::report(
                    if status == reqwest::StatusCode::UNAUTHORIZED {
                        super::ModelFailureKind::Authentication
                    } else {
                        super::ModelFailureKind::Forbidden
                    },
                    format!(
                        "Model API returned HTTP {status}: {}",
                        truncate_for_error(&body)
                    ),
                ));
            }
            if status != reqwest::StatusCode::TOO_MANY_REQUESTS && !status.is_server_error() {
                return Ok(response);
            }
            let retry_after = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(parse_retry_after_seconds);
            let body = read_response_text_with_timeout(
                response,
                header_timeout,
                "Model error response read failed",
                &self.url,
                diagnostics,
            )
            .await?;
            let delay = if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                if rate_retries >= 4 {
                    return Err(super::ModelRequestFailure::report(
                        super::ModelFailureKind::RateLimited,
                        format!(
                            "Model API returned HTTP {status}: {}",
                            truncate_for_error(&body)
                        ),
                    ));
                }
                let delay = retry_after.map_or_else(
                    || default_rate_limit_backoff(rate_retries),
                    Duration::from_secs,
                );
                rate_retries += 1;
                delay
            } else {
                if server_retries >= 3 {
                    return Err(super::ModelRequestFailure::report(
                        super::ModelFailureKind::Unavailable,
                        format!(
                            "Model API returned HTTP {status}: {}",
                            truncate_for_error(&body)
                        ),
                    ));
                }
                let delay = Duration::from_millis(400 * (1u64 << server_retries));
                server_retries += 1;
                delay
            };
            tracing::warn!(%status, delay_ms=delay.as_millis(), "Retrying model request");
            tokio::time::sleep(delay).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        core::{ModelProvider, ModelRequestOptions},
        model_protocol::{AgentMessage, AgentTurnRequest, PromptRequest},
    };
    use axum::{
        Json, Router,
        extract::State,
        response::{IntoResponse, Response},
        routing::post,
    };
    use daat_locus_macros::model_schema;
    use schemars::JsonSchema;
    use serde::{Deserialize, Serialize};
    use std::sync::atomic::{AtomicBool, Ordering};

    #[tokio::test]
    async fn rejected_credentials_are_failure_facts_without_retrying_the_request() {
        use crate::providers::{ModelFailureKind, ModelRequestFailure};
        for (status, expected) in [
            (
                reqwest::StatusCode::UNAUTHORIZED,
                ModelFailureKind::Authentication,
            ),
            (reqwest::StatusCode::FORBIDDEN, ModelFailureKind::Forbidden),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let requested = calls.clone();
            let app = Router::new().route(
                "/v1/chat/completions",
                post(move || {
                    let requested = requested.clone();
                    async move {
                        requested.fetch_add(1, Ordering::SeqCst);
                        (
                            status,
                            Json(serde_json::json!({"error":"invalid or expired credential"})),
                        )
                    }
                }),
            );
            let server = tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            let mut provider = super::super::OpenAIClient::from_parts(
                "fixture-secret",
                &format!("http://{address}/v1"),
                &ModelConfig::default(),
            );
            provider.transport.client = reqwest::Client::builder().no_proxy().build().unwrap();
            let request = AgentTurnRequest {
                messages: vec![AgentMessage::user("Test identity")],
                tools: vec![],
            };
            let options = ModelRequestOptions::for_agent_turn(&provider, &request, None).unwrap();
            let failure = match provider.complete_agent_turn(request, options).await {
                Err(failure) => failure,
                Ok(_) => panic!("rejected credentials must not produce a model answer"),
            };
            let fact = failure.downcast_ref::<ModelRequestFailure>().unwrap();
            assert_eq!(fact.kind, expected);
            assert!(!crate::core::should_retry_agent_turn_error(&failure));
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            server.abort();
        }
    }
    #[model_schema]
    #[derive(Serialize, Deserialize, JsonSchema)]
    struct Verdict {
        decision: String,
        reason: String,
    }

    #[derive(Clone)]
    struct Gateway {
        requests: Arc<parking_lot::Mutex<Vec<(HeaderMap, serde_json::Value)>>>,
        retry_review: Arc<AtomicBool>,
    }

    async fn gateway(
        State(state): State<Gateway>,
        headers: HeaderMap,
        Json(body): Json<serde_json::Value>,
    ) -> Response {
        for name in [
            "x-opencode-session",
            "x-opencode-request",
            "x-opencode-client",
            "x-opencode-project",
            "user-agent",
        ] {
            if !headers.contains_key(name) {
                return reqwest::StatusCode::BAD_REQUEST.into_response();
            }
        }
        assert_eq!(headers[AUTHORIZATION], "Bearer fixture-secret");
        state.requests.lock().push((headers, body.clone()));
        if body["tools"][0]["function"]["name"] == "review_app_call" {
            assert!(!body.to_string().contains("private worker history"));
            if state.retry_review.swap(false, Ordering::SeqCst) {
                return (
                    reqwest::StatusCode::TOO_MANY_REQUESTS,
                    [("retry-after", "0")],
                    Json(serde_json::json!({"error":"retry fixture"})),
                )
                    .into_response();
            }
            return Json(serde_json::json!({"choices":[{"message":{"tool_calls":[{"id":"verdict","type":"function","function":{"name":"review_app_call","arguments":"{\"decision\":\"approve\",\"reason\":\"requested\"}"}}]}}]})).into_response();
        }
        Json(serde_json::json!({"choices":[{"message":{"role":"assistant","content":"completed"},"finish_reason":"stop"}]})).into_response()
    }

    #[tokio::test]
    async fn main_and_independent_json_share_the_gateway_boundary_and_retry_identity() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let state = Gateway {
            requests: Arc::new(parking_lot::Mutex::new(vec![])),
            retry_review: Arc::new(AtomicBool::new(true)),
        };
        let app = Router::new()
            .route("/v1/chat/completions", post(gateway))
            .with_state(state.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let base = format!("http://opencode.ai:{}/v1", address.port());
        let model = ModelConfig {
            provider: "fixture".into(),
            model_id: "fixture".into(),
            ..Default::default()
        };
        let mut provider = super::super::OpenAIClient::from_parts("fixture-secret", &base, &model);
        provider.transport.client = reqwest::Client::builder()
            .no_proxy()
            .resolve("opencode.ai", address)
            .build()
            .unwrap();
        let agent = AgentTurnRequest {
            messages: vec![AgentMessage::user("private worker history")],
            tools: vec![],
        };
        let options =
            ModelRequestOptions::for_agent_turn(&provider, &agent, Some("worker".into())).unwrap();
        provider
            .complete_agent_turn(agent.clone(), options.clone())
            .await
            .unwrap();
        for _ in 0..2 {
            let review = PromptRequest {
                tool_name: "review_app_call".into(),
                tool_description: "Review".into(),
                output_schema: crate::schema_utils::model_schema_for::<Verdict>(),
                system_messages: vec!["Independent reviewer".into()],
                history_messages: vec![],
                retry_messages: vec![],
                current_user_message: "Read the requested file".into(),
            };
            let options = ModelRequestOptions::for_prompt(&provider, &review, None).unwrap();
            assert_eq!(
                provider.complete_json(review, options).await.unwrap()["decision"],
                "approve"
            );
        }
        provider.complete_agent_turn(agent, options).await.unwrap();
        let requests = state.requests.lock();
        assert_eq!(requests.len(), 5);
        assert_eq!(
            requests[0].0["x-opencode-session"],
            requests[4].0["x-opencode-session"]
        );
        assert_eq!(
            requests[1].0["x-opencode-session"],
            requests[2].0["x-opencode-session"]
        );
        assert_eq!(
            requests[1].0["x-opencode-request"],
            requests[2].0["x-opencode-request"]
        );
        assert_ne!(
            requests[0].0["x-opencode-session"],
            requests[1].0["x-opencode-session"]
        );
        assert_ne!(
            requests[1].0["x-opencode-session"],
            requests[3].0["x-opencode-session"]
        );
        server.abort();
    }
}
