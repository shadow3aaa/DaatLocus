use crate::support::Fixture;
use axum::{
    Json,
    http::HeaderMap,
    routing::{get, post},
};
use daat_locus::{
    core::ModelRequestOptions,
    model_protocol::{AgentMessage, AgentTurnRequest, PromptRequest},
};
use reqwest::StatusCode;
use serde_json::{Value, json};

fn check_identity(headers: &HeaderMap) {
    assert_eq!(headers["authorization"], "Bearer fixture-codex-key");
    assert_eq!(headers["originator"], "codex_cli_rs");
    assert!(
        headers["user-agent"]
            .to_str()
            .unwrap()
            .starts_with("codex_cli_rs/")
    );
    assert!(!headers.contains_key("chatgpt-account-id"));
}

#[tokio::test]
async fn custom_codex_key_supports_private_configuration_discovery_and_both_request_kinds() {
    let fixture = Fixture::new().await;
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let router = axum::Router::new().route("/v1/models", get(|headers: HeaderMap| async move {
        check_identity(&headers);
        Json(json!({"data":[{"id":"test-codex-model"}]}))
    })).route("/v1/responses", post(|headers: HeaderMap, Json(payload): Json<Value>| async move {
        check_identity(&headers);
        assert_eq!(payload["store"], false);
        assert_eq!(payload["stream"], true);
        assert_eq!(payload["max_output_tokens"], 512);
        let text = if payload.get("text").is_some() {
            assert_eq!(payload["text"]["format"]["type"], "json_schema");
            r#"{"decision":"approve"}"#
        } else {
            assert!(!payload["client_metadata"]["x-codex-installation-id"].as_str().unwrap().is_empty());
            "OK"
        };
        let event = json!({"type":"response.completed","response":{"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]}]}});
        ([("content-type", "text/event-stream")], format!("data: {event}\n\n"))
    }));
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    let read = || fixture.owner(reqwest::Method::GET, "/v1/config");
    let mut config: Value = read().send().await.unwrap().json().await.unwrap();
    config["providers"] = json!([{"kind":"codex","name":"mock","base_url":endpoint,"api_key":"fixture-codex-key","codex_auth_method":"api_key"}]);
    config["models"][0]["max_completion_tokens"] = json!(512);
    let saved = fixture
        .owner(reqwest::Method::PUT, "/v1/config")
        .json(&config)
        .send()
        .await
        .unwrap();
    assert_eq!(saved.status(), StatusCode::OK);
    let mut current: Value = read().send().await.unwrap().json().await.unwrap();
    assert_eq!(current["providers"][0]["kind"], "codex");
    assert_eq!(current["providers"][0]["codex_auth_method"], "api_key");
    assert_eq!(current["providers"][0]["credential_configured"], true);
    assert!(current["providers"][0]["api_key"].is_null());
    assert!(current["providers"][0]["codex_auth_file"].is_null());
    assert!(!current.to_string().contains("fixture-codex-key"));
    assert_eq!(
        fixture.system.safe_error("credential=fixture-codex-key"),
        "credential=[redacted]"
    );

    current["providers"][0]["name"] = json!("gateway");
    current["models"][0]["provider_name"] = json!("gateway");
    let saved = fixture
        .owner(reqwest::Method::PUT, "/v1/config")
        .json(&current)
        .send()
        .await
        .unwrap();
    assert_eq!(
        saved.status(),
        StatusCode::OK,
        "blank keys retain the saved credential across renaming"
    );
    current = read().send().await.unwrap().json().await.unwrap();
    let mut provider = current["providers"][0].clone();
    let discovery = fixture
        .owner(reqwest::Method::POST, "/v1/config/models/discover")
        .json(&json!({"provider":provider}))
        .send()
        .await
        .unwrap();
    assert_eq!(discovery.status(), StatusCode::OK);
    let models: Value = discovery.json().await.unwrap();
    assert_eq!(models["source"], "provider");
    assert_eq!(models["models"][0]["id"], "test-codex-model");
    provider["base_url"] = json!(format!("{endpoint}/changed"));
    assert!(
        !fixture
            .owner(reqwest::Method::POST, "/v1/config/models/discover")
            .json(&json!({"provider":provider}))
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );

    let saved = fixture.system.config();
    let model = daat_locus::providers::build_model_provider(&saved.main_model, &saved).unwrap();
    let request = AgentTurnRequest {
        messages: vec![
            AgentMessage::system("Be concise"),
            AgentMessage::user("Say OK"),
        ],
        tools: vec![],
    };
    let options = ModelRequestOptions::for_agent_turn(
        model.as_ref(),
        &request,
        Some("codex-api-test".into()),
    )
    .unwrap();
    assert_eq!(
        model
            .complete_agent_turn(request, options)
            .await
            .unwrap()
            .last_assistant_message
            .as_deref(),
        Some("OK")
    );
    let request = PromptRequest {
        tool_name: "review".into(),
        tool_description: "Review".into(),
        output_schema: json!({"type":"object","properties":{"decision":{"type":"string"}},"required":["decision"],"additionalProperties":false}),
        system_messages: vec![],
        history_messages: vec![],
        retry_messages: vec![],
        current_user_message: "Review".into(),
    };
    let options = ModelRequestOptions::for_prompt(model.as_ref(), &request, None).unwrap();
    assert_eq!(
        model.complete_json(request, options).await.unwrap(),
        json!({"decision":"approve"})
    );
    server.abort();
}
