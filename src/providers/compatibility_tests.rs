use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use axum::{Json, Router, extract::State, response::IntoResponse, routing::post};
use parking_lot::Mutex;
use serde_json::{Value, json};

use super::*;

#[derive(Clone, Default)]
struct Gateway {
    requests: Arc<Mutex<Vec<Value>>>,
    reject_thinking: Arc<AtomicBool>,
    payment_required: Arc<AtomicBool>,
    reject_no_tools: Arc<AtomicBool>,
    successful_barrier: Arc<Mutex<Option<Arc<tokio::sync::Barrier>>>>,
}

async fn respond(
    State(state): State<Gateway>,
    Json(payload): Json<Value>,
) -> axum::response::Response {
    state.requests.lock().push(payload.clone());
    if state.payment_required.load(Ordering::SeqCst) {
        return (axum::http::StatusCode::PAYMENT_REQUIRED, "payment required").into_response();
    }
    if state.reject_no_tools.load(Ordering::SeqCst)
        && payload["tools"].as_array().is_some_and(Vec::is_empty)
    {
        return (axum::http::StatusCode::BAD_REQUEST, "invalid request").into_response();
    }
    if payload["tools"][0]["function"].get("strict").is_some()
        || payload.get("tool_choice").is_some()
    {
        return (axum::http::StatusCode::BAD_REQUEST, "unsupported parameter").into_response();
    }
    if state.reject_thinking.load(Ordering::SeqCst) {
        if payload.get("reasoning_effort").is_some() {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                "unknown parameter: 'reasoning_effort'",
            )
                .into_response();
        }
        if payload.get("reasoning").is_some() {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                "unknown parameter: 'reasoning'",
            )
                .into_response();
        }
    }
    let barrier = state.successful_barrier.lock().clone();
    if let Some(barrier) = barrier {
        barrier.wait().await;
    }
    let message = if payload["stream"] == true {
        json!({"role":"assistant", "content":"completed"})
    } else {
        // A deny is a successful model result, not a reason to renegotiate.
        json!({"tool_calls":[{"id":"verdict", "type":"function", "function":{"name":"review", "arguments":"{\"decision\":\"deny\"}"}}]})
    };
    Json(json!({"choices":[{"message":message,"finish_reason":"stop"}]})).into_response()
}

async fn server() -> (String, Gateway, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let state = Gateway::default();
    let app = Router::new()
        .fallback(post(respond))
        .with_state(state.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (base, state, server)
}

fn client(base: &str, model: &ModelConfig) -> OpenAIClient {
    let mut client = OpenAIClient::from_parts("fixture", base, model);
    client
        .transport
        .set_test_client(crate::http_client::builder().no_proxy().build().unwrap());
    client
}

async fn review(client: &OpenAIClient) -> Value {
    let request = PromptRequest {
        tool_name: "review".into(),
        tool_description: "Review".into(),
        output_schema: json!({"type":"object","properties":{"decision":{"type":"string"}},"required":["decision"],"additionalProperties":false}),
        system_messages: vec![],
        history_messages: vec![],
        retry_messages: vec![],
        current_user_message: "Review".into(),
    };
    let options = ModelRequestOptions::for_prompt(client, &request, None).unwrap();
    client.complete_json(request, options).await.unwrap()
}

#[tokio::test]
async fn successful_downgrades_survive_instances_and_later_400s_across_request_kinds() {
    let (base, state, server) = server().await;
    let model = ModelConfig {
        thinking_budget: Some(crate::config::ThinkingBudget::new("medium")),
        ..Default::default()
    };
    assert_eq!(
        review(&client(&base, &model)).await,
        json!({"decision":"deny"})
    );
    assert_eq!(state.requests.lock().len(), 4);
    assert_eq!(
        review(&client(&base, &model)).await,
        json!({"decision":"deny"})
    );
    assert_eq!(state.requests.lock().len(), 5);

    state.reject_thinking.store(true, Ordering::SeqCst);
    let provider = client(&base, &model);
    let request = AgentTurnRequest {
        messages: vec![AgentMessage::user("Work")],
        tools: vec![AgentToolSpec {
            name: "work".into(),
            description: "Work".into(),
            input_spec: AgentToolInputSpec::JsonSchema {
                schema: json!({"type":"object","properties":{},"required":[],"additionalProperties":false}),
            },
        }],
    };
    let options = ModelRequestOptions::for_agent_turn(&provider, &request, None).unwrap();
    provider
        .complete_agent_turn(request, options)
        .await
        .unwrap();
    assert_eq!(
        state.requests.lock().len(),
        8,
        "agent reuses strict/tool-choice choices, then learns thinking fallback"
    );
    assert_eq!(
        review(&client(&base, &model)).await,
        json!({"decision":"deny"})
    );
    let requests = state.requests.lock();
    assert_eq!(requests.len(), 9);
    assert!(requests.last().unwrap().get("reasoning").is_none());
    assert!(requests.last().unwrap().get("reasoning_effort").is_none());
    drop(requests);
    server.abort();
}

async fn concurrent_reviews(base: &str, model: &ModelConfig) {
    let barrier = Arc::new(tokio::sync::Barrier::new(8));
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..8 {
        let base = base.to_owned();
        let model = model.clone();
        let barrier = barrier.clone();
        tasks.spawn(async move {
            let provider = client(&base, &model);
            barrier.wait().await;
            review(&provider).await
        });
    }
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(result) = tasks.join_next().await {
            assert_eq!(result.unwrap(), json!({"decision":"deny"}));
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn concurrent_clients_singleflight_probes_but_not_warm_requests() {
    let (base, state, server) = server().await;
    let model = ModelConfig {
        thinking_budget: Some(crate::config::ThinkingBudget::new("medium")),
        ..Default::default()
    };
    concurrent_reviews(&base, &model).await;
    assert_eq!(
        state.requests.lock().len(),
        11,
        "only three failed probes plus eight successes"
    );

    // If cache hits serialize their HTTP requests, none can pass this server barrier.
    *state.successful_barrier.lock() = Some(Arc::new(tokio::sync::Barrier::new(8)));
    concurrent_reviews(&base, &model).await;
    assert_eq!(state.requests.lock().len(), 19);
    *state.successful_barrier.lock() = None;
    state.requests.lock().clear();
    state.reject_thinking.store(true, Ordering::SeqCst);
    concurrent_reviews(&base, &model).await;
    let requests = state.requests.lock();
    assert_eq!(
        requests
            .iter()
            .filter(|p| p.get("reasoning").is_some())
            .count(),
        1,
        "only the probe owner explores the nested-thinking fallback after a warm 400"
    );
    assert_eq!(
        requests
            .iter()
            .filter(|p| p.get("reasoning").is_none() && p.get("reasoning_effort").is_none())
            .count(),
        8
    );
    drop(requests);
    server.abort();
}

#[tokio::test]
async fn unsuccessful_http_requests_do_not_publish_downgrades() {
    let (base, state, server) = server().await;
    state.payment_required.store(true, Ordering::SeqCst);
    let provider = client(&base, &ModelConfig::default());
    let request = PromptRequest {
        tool_name: "review".into(),
        tool_description: "Review".into(),
        output_schema: json!({"type":"object","properties":{},"required":[],"additionalProperties":false}),
        system_messages: vec![],
        history_messages: vec![],
        retry_messages: vec![],
        current_user_message: "Review".into(),
    };
    let options = ModelRequestOptions::for_prompt(&provider, &request, None).unwrap();
    assert!(provider.complete_json(request, options).await.is_err());
    assert_eq!(state.requests.lock().len(), 1);
    state.payment_required.store(false, Ordering::SeqCst);
    assert_eq!(
        review(&client(&base, &ModelConfig::default())).await,
        json!({"decision":"deny"})
    );
    assert_eq!(state.requests.lock().len(), 5);
    server.abort();
}

#[tokio::test]
async fn no_tool_400_does_not_disable_tool_features_for_other_requests() {
    let (base, state, server) = server().await;
    state.reject_no_tools.store(true, Ordering::SeqCst);
    let provider = client(&base, &ModelConfig::default());
    let request = AgentTurnRequest {
        messages: vec![AgentMessage::user("Work")],
        tools: vec![],
    };
    let options = ModelRequestOptions::for_agent_turn(&provider, &request, None).unwrap();
    assert!(
        provider
            .complete_agent_turn(request, options)
            .await
            .is_err()
    );
    assert_eq!(state.requests.lock().len(), 1);
    assert_eq!(
        review(&client(&base, &ModelConfig::default())).await,
        json!({"decision":"deny"})
    );
    assert_eq!(
        state.requests.lock().len(),
        5,
        "a later request still explores strict and tool_choice independently of an irrelevant 400"
    );
    server.abort();
}

#[tokio::test]
async fn responses_reuses_summary_and_vision_fallbacks_across_instances() {
    async fn respond_responses(
        State(state): State<Gateway>,
        Json(payload): Json<Value>,
    ) -> axum::response::Response {
        state.requests.lock().push(payload.clone());
        if payload["reasoning"].get("summary").is_some() {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                "reasoning.summary is unsupported",
            )
                .into_response();
        }
        let has_image = payload["input"].as_array().unwrap().iter().any(|item| {
            item["content"]
                .as_array()
                .is_some_and(|parts| parts.iter().any(|part| part["type"] == "input_image"))
        });
        if state.reject_no_tools.load(Ordering::SeqCst) && has_image {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                "unknown variant input_image",
            )
                .into_response();
        }
        let text = if payload.get("text").is_some() {
            "{\"decision\":\"deny\"}"
        } else {
            "completed"
        };
        let event = json!({"type":"response.completed", "response":{"status":"completed", "output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]}]}});
        (
            [("content-type", "text/event-stream")],
            format!("data: {event}\n\n"),
        )
            .into_response()
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let state = Gateway::default();
    let app = Router::new()
        .fallback(post(respond_responses))
        .with_state(state.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let model = ModelConfig {
        supports_vision: Some(true),
        ..Default::default()
    };
    let request = AgentTurnRequest {
        messages: vec![AgentMessage::user_content(AgentContent::multimodal(
            "Inspect",
            vec![AgentContentPart::ImageData {
                media_type: "image/png".into(),
                data_base64: "iVBORw0KGgo=".into(),
                description: None,
            }],
        ))],
        tools: vec![],
    };
    for expected_requests in [2, 3, 5, 6] {
        if expected_requests == 5 {
            state.reject_no_tools.store(true, Ordering::SeqCst);
        }
        let mut provider =
            responses_compat::ResponsesCompatibleClient::new("fixture", &base, &model);
        provider.set_test_client(crate::http_client::builder().no_proxy().build().unwrap());
        let options = ModelRequestOptions::for_agent_turn(&provider, &request, None).unwrap();
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            provider.complete_agent_turn(request.clone(), options),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result.last_assistant_message.as_deref(), Some("completed"));
        assert_eq!(state.requests.lock().len(), expected_requests);
    }
    let requests = state.requests.lock();
    assert!(requests[5]["reasoning"].get("summary").is_none());
    assert!(
        requests[5]["input"][0]["content"]
            .as_array()
            .unwrap()
            .iter()
            .all(|part| part["type"] != "input_image")
    );
    drop(requests);
    server.abort();
}
