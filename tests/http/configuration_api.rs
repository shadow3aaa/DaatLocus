//! Configuration HTTP contracts.
use crate::support::Fixture;
use daat_locus::actors::ApprovalMode;
use daat_locus::config::ProviderConfig;
use reqwest::StatusCode;
use serde_json::{Value, json};
use std::sync::Arc;

#[tokio::test]
async fn configuration_preview_accepts_provider_rename_without_disclosing_credentials() {
    let fixture = Fixture::new().await;
    let mut config: Value = fixture
        .owner(reqwest::Method::GET, "/v1/config")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(config["providers"][0]["api_key"], Value::Null);
    assert_eq!(config["providers"][0]["credential_name"], "mock");
    assert!(!config.to_string().contains("fixture-secret-key"));
    config["providers"][0]["name"] = json!("renamed-provider");
    config["models"][0]["provider_name"] = json!("renamed-provider");
    let preview = fixture
        .owner(reqwest::Method::POST, "/v1/config/preview")
        .json(&config)
        .send()
        .await
        .unwrap();
    assert_eq!(
        preview.status(),
        StatusCode::OK,
        "{}",
        preview.text().await.unwrap()
    );
}

#[tokio::test]
async fn actor_usage_requires_owner_and_starts_with_no_input_tokens() {
    let fixture = Fixture::new().await;
    let usage = fixture
        .owner(reqwest::Method::GET, "/v1/actors/root/usage")
        .send()
        .await
        .unwrap();
    assert_eq!(usage.status(), StatusCode::OK);
    assert_eq!(
        usage.json::<Value>().await.unwrap()["total_token_usage"]["input_tokens"],
        0
    );
    assert_eq!(
        fixture
            .client
            .get(format!("{}/v1/actors/root/usage", fixture.url))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn configured_model_discovery_uses_private_credentials_only_at_the_bound_endpoint() {
    let fixture = Fixture::new().await;
    let contacted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = contacted.clone();
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let fake_provider = axum::Router::new().route(
        "/v1/models",
        axum::routing::get(move |headers: axum::http::HeaderMap| {
            let seen = seen.clone();
            async move {
                assert_eq!(headers["authorization"], "Bearer fixture-secret-key");
                seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                axum::Json(json!({"data":[{"id":"local-discovered-model"}]}))
            }
        }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, fake_provider).await.unwrap();
    });
    let mut config = fixture.system.config();
    config.providers.insert(
        "mock".into(),
        ProviderConfig::OpenaiCompatible {
            base_url: endpoint.clone(),
            api_key: "fixture-secret-key".into(),
        },
    );
    fixture.system.update_config(config).await.unwrap();
    let response = fixture.owner(reqwest::Method::POST, "/v1/config/models/discover")
        .json(&json!({"provider":{"kind":"openai_compatible", "name":"mock", "base_url":endpoint, "api_key":null}}))
        .send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let text = response.text().await.unwrap();
    assert!(text.contains("local-discovered-model") && !text.contains("fixture-secret-key"));
    assert_eq!(contacted.load(std::sync::atomic::Ordering::SeqCst), 1);
    let response = fixture.owner(reqwest::Method::POST, "/v1/config/models/discover")
        .json(&json!({"provider":{"kind":"openai_compatible", "name":"mock", "base_url":format!("{endpoint}/changed"), "api_key":null}}))
        .send().await.unwrap();
    assert!(!response.status().is_success());
    assert_eq!(contacted.load(std::sync::atomic::Ordering::SeqCst), 1);
    server.abort();
}

#[tokio::test]
async fn composer_preferences_patch_preserves_latest_grants_and_unspecified_mode() {
    let fixture = Fixture::new().await;
    let actor = fixture.child("preference-project").await;
    let mut config = fixture.system.config();
    config
        .models
        .insert("alternate".into(), config.models["mock"].clone());
    fixture.system.update_config(config).await.unwrap();
    fixture
        .system
        .patch_owned_preferences("core", None, Some(ApprovalMode::Ask))
        .await
        .unwrap();
    let mut tightened = fixture.system.actor(&actor).unwrap().config;
    let extra = fixture.system.device_workspace().join("extra-read-access");
    std::fs::create_dir(&extra).unwrap();
    let extra = extra.canonicalize().unwrap();
    tightened.permissions.readable_roots.push(extra.clone());
    tightened.permissions.readable_roots.sort();
    tightened.permissions.approval_mode = ApprovalMode::Ask;
    fixture
        .system
        .update_owned(&actor, tightened.clone())
        .await
        .unwrap();
    let response = fixture
        .owner(reqwest::Method::PATCH, &format!("/v1/actors/{actor}"))
        .json(&json!({"model":"alternate"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let saved = fixture.system.actor(&actor).unwrap().config;
    assert_eq!(saved.permissions, tightened.permissions);
    assert_eq!(saved.model.as_deref(), Some("alternate"));
    let response = fixture
        .owner(reqwest::Method::PATCH, "/v1/actors/core")
        .json(&json!({"approval_mode":"none"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let saved = fixture.system.actor(&actor).unwrap().config;
    assert_eq!(saved.model.as_deref(), Some("alternate"));
    assert_eq!(
        saved.permissions.readable_roots,
        tightened.permissions.readable_roots
    );
    assert!(saved.permissions.writable_roots.contains(&saved.workspace));
    assert_eq!(saved.permissions.approval_mode, ApprovalMode::None);
    let response = fixture
        .owner(reqwest::Method::PATCH, &format!("/v1/actors/{actor}"))
        .json(&json!({"model":null}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(fixture.system.actor(&actor).unwrap().config.model, None);
    for patch in [
        json!({}),
        json!({"permissions":{"writable_roots":["C:/"]}}),
        json!({"approval_mode":null}),
        json!({"model":42}),
    ] {
        assert_eq!(
            fixture
                .owner(reqwest::Method::PATCH, &format!("/v1/actors/{actor}"))
                .json(&patch)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
}

#[tokio::test]
async fn removing_a_model_resets_old_actor_preferences_and_persists_in_the_service_profile() {
    let fixture = Fixture::new().await;
    let mut current = fixture.system.config();
    current
        .models
        .insert("specialized".into(), current.models["mock"].clone());
    fixture.system.update_config(current).await.unwrap();
    let actor = fixture.child("specialized-project").await;
    let mut config = fixture.system.actor(&actor).unwrap().config;
    config.model = Some("specialized".into());
    fixture.system.update_owned(&actor, config).await.unwrap();
    let permissions = fixture.system.actor(&actor).unwrap().config.permissions;
    let response = fixture.owner(reqwest::Method::PUT, "/v1/config").json(&json!({
        "providers": [{"kind": "openai_compatible", "name": "mock", "api_key": "fixture-secret-key", "base_url": "http://model.invalid/v1"}],
        "models": [{"name": "mock", "provider_name": "mock", "model_id": "mock"}],
        "main_model": "mock", "approval_model": null,
    })).send().await.unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        response.text().await.unwrap()
    );
    assert!(!fixture.system.config().models.contains_key("specialized"));
    assert_eq!(
        fixture
            .system
            .actor(&actor)
            .unwrap()
            .config
            .model
            .as_deref(),
        None
    );
    assert_eq!(
        fixture.system.actor(&actor).unwrap().config.permissions,
        permissions
    );
    let directory = fixture
        .system
        .infrastructure_dir()
        .parent()
        .unwrap()
        .join("config");
    let saved: daat_locus::config::Config =
        toml::from_str(&std::fs::read_to_string(directory.join("config.toml")).unwrap()).unwrap();
    assert_eq!(saved.main_model, "mock");
    assert!(!saved.models.contains_key("specialized"));
    assert_eq!(
        std::fs::read(directory.join("config.toml")).unwrap(),
        std::fs::read(directory.join("config.toml.bak")).unwrap()
    );
}

#[tokio::test]
async fn the_last_provider_can_be_removed_without_deleting_actors_or_queued_inputs() {
    let fixture = Fixture::new().await;
    let actor = fixture.child("kept-project").await;
    let mut preference = fixture.system.actor(&actor).unwrap().config;
    preference.model = Some("mock".into());
    fixture
        .system
        .update_owned(&actor, preference)
        .await
        .unwrap();
    let queued = fixture
        .system
        .send_user(&actor, "Keep this input".into(), vec![], None)
        .unwrap();
    let mut request: Value = fixture
        .owner(reqwest::Method::GET, "/v1/config")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    // A caller can remove the provider alone; the backend cascades its old models.
    request["providers"] = json!([]);
    let response = fixture
        .owner(reqwest::Method::PUT, "/v1/config")
        .json(&request)
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        response.text().await.unwrap()
    );
    assert!(fixture.system.config().models.is_empty());
    assert!(fixture.system.config().main_model.is_empty());
    assert!(fixture.system.actor(&actor).unwrap().config.model.is_none());
    assert_eq!(
        fixture.system.store().message(&queued.id).unwrap().status,
        daat_locus::actors::MessageStatus::Queued
    );
    assert_eq!(
        fixture.system.actor(&actor).unwrap().config.name,
        "kept-project"
    );
    let readiness: Value = fixture
        .owner(reqwest::Method::GET, "/v1/config/readiness")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(readiness["ready"], false);
    let record = fixture
        .stage
        .host
        .configurations
        .current()
        .unwrap()
        .unwrap();
    assert!(
        daat_locus::accounts::configuration::ConfigurationStore::candidate(
            &fixture.stage.host,
            &record
        )
        .unwrap()
        .models
        .is_empty()
    );
    fixture.system.shutdown().await;
}

#[tokio::test]
async fn readiness_reads_effective_service_configuration_without_global_file_repairs() {
    let ready = Fixture::new().await;
    let empty = Fixture::with_config(Default::default()).await;
    for (fixture, expected) in [(&ready, true), (&empty, false)] {
        let report: Value = fixture
            .owner(reqwest::Method::GET, "/v1/config/readiness")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(report["ready"], expected);
        assert_eq!(report["recovered"], false);
    }
    empty
        .system
        .update_config(ready.system.config())
        .await
        .unwrap();
    let report: Value = empty
        .owner(reqwest::Method::GET, "/v1/config/readiness")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(report["ready"], true);
    ready.system.shutdown().await;
    empty.system.shutdown().await;
}
