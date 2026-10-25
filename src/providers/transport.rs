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
    #[cfg(test)]
    pub(super) fn set_test_client(&mut self, client: reqwest::Client) {
        self.client = client;
    }
    pub(super) fn new(base_url: &str, model: &ModelConfig) -> Self {
        Self {
            client: crate::http_client::builder()
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
                if let Some(error) = super::ModelRequestFailure::opencode_response(
                    &self.http.base_url,
                    status,
                    &body,
                ) {
                    return Err(error);
                }
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
                if status.is_client_error() {
                    tracing::debug!(
                        %status,
                        url = %self.url,
                        "Model request returned a non-retryable client error status"
                    );
                }
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
            if let Some(error) =
                super::ModelRequestFailure::opencode_response(&self.http.base_url, status, &body)
                && !error
                    .downcast_ref::<super::ModelRequestFailure>()
                    .unwrap()
                    .retryable()
            {
                return Err(error);
            }
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
    async fn streaming_protocols_emit_each_text_fragment_once_across_requests() {
        use crate::core::{ModelProgressEvent, ModelProgressSink};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new().fallback(post(|uri: axum::http::Uri| async move {
            let mut events = vec![];
            let assistant = "段🙂".repeat(32);
            let reasoning = "思🙂".repeat(32);
            let tail = if uri.path().contains("aligned") { "" } else { " 尾\n" };
            let chunks = [assistant.as_str(), assistant.as_str(), tail];
            let thoughts = [reasoning.as_str(), reasoning.as_str(), tail];
            if uri.path().ends_with("/messages") {
                events.push(serde_json::json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking"}}));
                events.push(serde_json::json!({"type":"content_block_start","index":1,"content_block":{"type":"text"}}));
                for (text, thinking) in chunks.iter().zip(thoughts) {
                    events.push(serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":thinking}}));
                    events.push(serde_json::json!({"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":text}}));
                }
                events.push(serde_json::json!({"type":"message_stop"}));
            } else if uri.path().ends_with("/responses") {
                for (text, thinking) in chunks.iter().zip(thoughts) {
                    events.push(serde_json::json!({"type":"response.reasoning_text.delta","delta":thinking}));
                    events.push(serde_json::json!({"type":"response.output_text.delta","delta":text}));
                }
                events.push(serde_json::json!({"type":"response.completed","response":{"output":[]}}));
            } else if uri.path().ends_with("/api/chat") {
                for (text, thinking) in chunks.iter().zip(thoughts) {
                    events.push(serde_json::json!({"message":{"content":text,"thinking":thinking},"done":false}));
                }
                events.push(serde_json::json!({"done":true}));
                return ([("content-type", "application/x-ndjson")], events.iter().map(|event| format!("{event}\n")).collect::<String>());
            } else {
                for (text, thinking) in chunks.iter().zip(thoughts) {
                    events.push(serde_json::json!({"choices":[{"delta":{"content":text,"reasoning_content":thinking}}]}));
                }
                events.push(serde_json::json!({"choices":[{"delta":{},"finish_reason":"stop"}]}));
            }
            let mut body = events.iter().map(|event| format!("data: {event}\n\n")).collect::<String>();
            body.push_str("data: [DONE]\n\n");
            ([("content-type", "text/event-stream")], body)
        }));
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        for path in ["zen/v1", "go/v1", "aligned/v1"] {
            let base = format!("http://opencode.ai:{}/{path}", address.port());
            let client = crate::http_client::builder()
                .no_proxy()
                .resolve("opencode.ai", address)
                .build()
                .unwrap();
            let model = ModelConfig::default();
            let mut chat = super::super::OpenAIClient::from_parts("fixture-key", &base, &model);
            chat.transport.client = client.clone();
            let mut responses = super::super::responses_compat::ResponsesCompatibleClient::new(
                "fixture-key",
                &base,
                &model,
            );
            responses.set_test_client(client.clone());
            let mut anthropic = super::super::anthropic_compat::AnthropicCompatibleClient::new(
                "fixture-key",
                &base,
                &model,
            );
            anthropic.set_test_client(client);
            let ollama_host = format!("http://{address}/{path}");
            let ollama =
                super::super::OllamaClient::from_parts(Some(&ollama_host), &model, None, None);
            let providers: Vec<Box<dyn ModelProvider>> = vec![
                Box::new(chat),
                Box::new(responses),
                Box::new(anthropic),
                Box::new(ollama),
            ];
            for provider in providers {
                for _ in 0..2 {
                    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
                    let request = AgentTurnRequest {
                        messages: vec![AgentMessage::user("Test streaming")],
                        tools: vec![],
                    };
                    let options =
                        ModelRequestOptions::for_agent_turn(provider.as_ref(), &request, None)
                            .unwrap()
                            .with_progress(Some(ModelProgressSink::new(tx)));
                    provider
                        .complete_agent_turn(request, options)
                        .await
                        .unwrap();
                    let mut assistant = vec![];
                    let mut reasoning = vec![];
                    while let Ok(event) = rx.try_recv() {
                        match event {
                            ModelProgressEvent::AssistantDelta { text } => assistant.push(text),
                            ModelProgressEvent::ReasoningDelta { text } => reasoning.push(text),
                            other => panic!("unexpected progress: {other:?}"),
                        }
                    }
                    let tail = if path.starts_with("aligned") {
                        ""
                    } else {
                        " 尾\n"
                    };
                    for (actual, token) in [(assistant, "段🙂"), (reasoning, "思🙂")] {
                        let expected = format!("{}{tail}", token.repeat(64));
                        assert_eq!(actual.concat(), expected, "protocol progress at {path}");
                    }
                }
            }
        }
        server.abort();
    }

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
            provider.transport.client = crate::http_client::builder().no_proxy().build().unwrap();
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

    #[tokio::test]
    async fn chat_finish_reason_completes_without_done_or_connection_close_and_retains_usage() {
        use crate::model_protocol::{AgentToolInputSpec, AgentToolSpec};
        use futures_util::StreamExt;
        for tool_call in [false, true] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let app = Router::new().fallback(post(move || async move {
                let delta = if tool_call {
                    serde_json::json!({"tool_calls":[{"index":0,"id":"call_done","type":"function","function":{"name":"review","arguments":"{\"decision\":\"approve\",\"reason\":\"ok\"}"}}]})
                } else { serde_json::json!({"content":"OK"}) };
                let first = format!("data: {}\n\n", serde_json::json!({"choices":[{"delta":delta,"finish_reason":if tool_call {"tool_calls"} else {"stop"}}]}));
                let usage = "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":3,\"total_tokens\":10}}\n\n";
                let body = futures_util::stream::once(async move { Ok::<_,std::io::Error>(axum::body::Bytes::from(first)) })
                    .chain(futures_util::stream::once(async move { tokio::time::sleep(Duration::from_millis(40)).await; Ok(axum::body::Bytes::from_static(usage.as_bytes())) }))
                    .chain(futures_util::stream::unfold((), |()| async { tokio::time::sleep(Duration::from_millis(20)).await; Some((Ok(axum::body::Bytes::from_static(b": heartbeat\n\n")),())) }));
                ([("content-type","text/event-stream")], axum::body::Body::from_stream(body))
            }));
            let server = tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            let mut provider = super::super::OpenAIClient::from_parts(
                "fixture-key",
                &format!("http://{address}/v1"),
                &ModelConfig::default(),
            );
            provider.transport.client = crate::http_client::builder().no_proxy().build().unwrap();
            let request = AgentTurnRequest {
                messages: vec![AgentMessage::user("Test completion")],
                tools: if tool_call {
                    vec![AgentToolSpec {
                        name: "review".into(),
                        description: "Review".into(),
                        input_spec: AgentToolInputSpec::JsonSchema {
                            schema: crate::schema_utils::model_schema_for::<Verdict>(),
                        },
                    }]
                } else {
                    vec![]
                },
            };
            let options = ModelRequestOptions::for_agent_turn(&provider, &request, None).unwrap();
            let response = tokio::time::timeout(
                Duration::from_secs(3),
                provider.complete_agent_turn(request, options),
            )
            .await
            .expect("finished generation must not await connection close")
            .unwrap();
            let response = response.protocol();
            if tool_call {
                assert_eq!(response.tool_calls.len(), 1);
                assert_eq!(response.tool_calls[0].arguments["decision"], "approve");
            } else {
                assert_eq!(response.final_assistant_message.as_deref(), Some("OK"));
            }
            assert_eq!(provider.token_usage_info().last_token_usage.input_tokens, 7);
            assert_eq!(
                provider.token_usage_info().last_token_usage.output_tokens,
                3
            );
            server.abort();
        }
    }

    #[tokio::test]
    async fn configured_timeouts_remain_typed_for_main_and_independent_requests() {
        use crate::providers::{ModelFailureKind, ModelRequestFailure};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new().fallback(post(|| async {
            (
                [("content-type", "text/event-stream")],
                axum::body::Body::from_stream(futures_util::stream::pending::<
                    Result<axum::body::Bytes, std::convert::Infallible>,
                >()),
            )
        }));
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let base = format!("http://{address}/v1");
        let model = ModelConfig {
            request_timeout_secs: 1,
            stream_idle_timeout_secs: 1,
            ..Default::default()
        };
        let client = crate::http_client::builder().no_proxy().build().unwrap();
        let mut chat = super::super::OpenAIClient::from_parts("fixture-key", &base, &model);
        chat.transport.client = client.clone();
        let mut responses = super::super::responses_compat::ResponsesCompatibleClient::new(
            "fixture-key",
            &base,
            &model,
        );
        responses.set_test_client(client.clone());
        let mut anthropic = super::super::anthropic_compat::AnthropicCompatibleClient::new(
            "fixture-key",
            &base,
            &model,
        );
        anthropic.set_test_client(client);
        let providers: Vec<Box<dyn ModelProvider>> =
            vec![Box::new(chat), Box::new(responses), Box::new(anthropic)];
        for provider in providers {
            let request = AgentTurnRequest {
                messages: vec![AgentMessage::user("Test")],
                tools: vec![],
            };
            let options =
                ModelRequestOptions::for_agent_turn(provider.as_ref(), &request, None).unwrap();
            let error = provider
                .complete_agent_turn(request, options)
                .await
                .err()
                .unwrap();
            assert_eq!(
                error.downcast_ref::<ModelRequestFailure>().unwrap().kind,
                ModelFailureKind::Timeout
            );
            let request = PromptRequest {
                tool_name: "review_app_call".into(),
                tool_description: "Review".into(),
                output_schema: crate::schema_utils::model_schema_for::<Verdict>(),
                system_messages: vec![],
                history_messages: vec![],
                retry_messages: vec![],
                current_user_message: "Review".into(),
            };
            let options =
                ModelRequestOptions::for_prompt(provider.as_ref(), &request, None).unwrap();
            let error = provider.complete_json(request, options).await.unwrap_err();
            assert_eq!(
                error.downcast_ref::<ModelRequestFailure>().unwrap().kind,
                ModelFailureKind::Timeout
            );
        }
        server.abort();
    }

    #[tokio::test]
    async fn opencode_direct_urls_classify_failures_across_all_protocols() {
        use crate::providers::{ModelFailureKind, ModelRequestFailure};
        for (status, error_type, message, expected) in [
            (
                402,
                "server_error",
                "Upstream request failed: Insufficient account funds",
                ModelFailureKind::Billing,
            ),
            (
                400,
                "server_error",
                "Upstream request failed: Model is unavailable.",
                ModelFailureKind::ModelUnavailable,
            ),
            (
                403,
                "RegionError",
                "This model is not available in your country",
                ModelFailureKind::RegionUnavailable,
            ),
            (
                400,
                "upstream_error",
                "Temporary upstream failure",
                ModelFailureKind::Unavailable,
            ),
            (
                503,
                "server_error",
                "Model is unavailable.",
                ModelFailureKind::ModelUnavailable,
            ),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let requested = calls.clone();
            let app = Router::new().fallback(post(move |headers: HeaderMap| {
                let requested = requested.clone();
                async move {
                    requested.fetch_add(1, Ordering::SeqCst);
                    assert_eq!(headers[AUTHORIZATION], "Bearer fixture-key");
                    assert!(!headers.contains_key("x-opencode-org-id"));
                    for name in [
                        "x-opencode-session",
                        "x-opencode-request",
                        "x-opencode-client",
                        "x-opencode-project",
                    ] {
                        assert!(headers.contains_key(name));
                    }
                    (
                        reqwest::StatusCode::from_u16(status).unwrap(),
                        Json(serde_json::json!({"error":{"type":error_type,"message":message}})),
                    )
                }
            }));
            let server = tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            for path in [
                "zen/v1",
                "go/v1",
                "inference/openai/v1",
                "inference/go/openai/v1",
            ] {
                let base = format!("http://opencode.ai:{}/{path}", address.port());
                let client = crate::http_client::builder()
                    .no_proxy()
                    .resolve("opencode.ai", address)
                    .build()
                    .unwrap();
                let model = ModelConfig::default();
                let mut chat = super::super::OpenAIClient::from_parts("fixture-key", &base, &model);
                chat.transport.client = client.clone();
                let mut responses = super::super::responses_compat::ResponsesCompatibleClient::new(
                    "fixture-key",
                    &base,
                    &model,
                );
                responses.set_test_client(client.clone());
                let mut anthropic = super::super::anthropic_compat::AnthropicCompatibleClient::new(
                    "fixture-key",
                    &base,
                    &model,
                );
                anthropic.set_test_client(client);
                let providers: Vec<Box<dyn ModelProvider>> =
                    vec![Box::new(chat), Box::new(responses), Box::new(anthropic)];
                for provider in providers {
                    let agent = AgentTurnRequest {
                        messages: vec![AgentMessage::user("Test")],
                        tools: vec![],
                    };
                    let options =
                        ModelRequestOptions::for_agent_turn(provider.as_ref(), &agent, None)
                            .unwrap();
                    let failure = provider
                        .complete_agent_turn(agent, options)
                        .await
                        .err()
                        .unwrap();
                    assert_eq!(
                        failure.downcast_ref::<ModelRequestFailure>().unwrap().kind,
                        expected
                    );
                    assert_eq!(
                        crate::core::should_retry_agent_turn_error(&failure),
                        expected == ModelFailureKind::Unavailable
                    );
                    let review = PromptRequest {
                        tool_name: "review_app_call".into(),
                        tool_description: "Review".into(),
                        output_schema: crate::schema_utils::model_schema_for::<Verdict>(),
                        system_messages: vec![],
                        history_messages: vec![],
                        retry_messages: vec![],
                        current_user_message: "Review".into(),
                    };
                    let options =
                        ModelRequestOptions::for_prompt(provider.as_ref(), &review, None).unwrap();
                    let failure = provider.complete_json(review, options).await.unwrap_err();
                    assert_eq!(
                        failure.downcast_ref::<ModelRequestFailure>().unwrap().kind,
                        expected
                    );
                }
            }
            assert_eq!(
                calls.load(Ordering::SeqCst),
                24,
                "known errors must not trigger parameter fallback or repeated requests"
            );
            server.abort();
        }
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
        provider.transport.client = crate::http_client::builder()
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
