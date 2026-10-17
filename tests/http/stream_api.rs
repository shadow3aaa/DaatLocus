//! Stream HTTP contracts.
use crate::support::Fixture;
use daat_locus::config::Config;
use reqwest::StatusCode;
use serde_json::{Value, json};
use std::time::Duration;

#[tokio::test]
async fn spa_fallback_never_masks_api_errors_or_unknown_paths() {
    let fixture = Fixture::new().await;
    for path in [
        "/v1/missing",
        "/v1/shares",
        "/share/removed",
        "/settings",
        "/settings/missing",
        "/settings/providers/extra",
        "/actors/root/extra",
        "/assets/unknown.js",
        "/%2e%2e/private",
    ] {
        let response = fixture
            .client
            .get(format!("{}{path}", fixture.url))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        assert!(
            response.headers()["content-type"]
                .to_str()
                .unwrap()
                .starts_with("application/json")
        );
    }
    for path in [
        "/",
        "/actors/root",
        "/settings/general",
        "/settings/appearance",
        "/settings/devices",
        "/settings/permissions",
        "/settings/providers",
    ] {
        let response = fixture
            .client
            .get(format!("{}{path}", fixture.url))
            .send()
            .await
            .unwrap();
        if response.status() == StatusCode::OK {
            assert!(
                response.headers()["content-type"]
                    .to_str()
                    .unwrap()
                    .starts_with("text/html")
            );
            assert!(response.headers().contains_key("content-security-policy"));
        } else {
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(
                response.json::<Value>().await.unwrap()["code"],
                "webui_not_built"
            );
        }
    }
}

#[tokio::test]
async fn sse_replays_from_last_event_id_and_revocation_closes_live_access() {
    let fixture = Fixture::new().await;
    let actor = fixture.child("alpha").await;
    let cookie = fixture.login_cookie().await;
    let event = |text: &str| daat_locus::actors::ActorEvent::GenerationDelta {
        message_id: "m".into(),
        generation: 1,
        channel: "assistant".into(),
        text: text.into(),
    };
    let first = fixture
        .system
        .store()
        .append_event(&actor, event("first"))
        .unwrap();
    let second = fixture
        .system
        .store()
        .append_event(&actor, event("second"))
        .unwrap();
    let mut response = fixture
        .client
        .get(format!(
            "{}/v1/actors/{actor}/event-stream?after=0&limit=1",
            fixture.url
        ))
        .header("cookie", &cookie)
        .header("last-event-id", first.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let chunk = tokio::time::timeout(Duration::from_secs(2), response.chunk())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let text = String::from_utf8_lossy(&chunk);
    assert!(
        text.contains(&format!("id: {second}"))
            && text.contains("second")
            && !text.contains("first"),
        "{text}"
    );
    assert_eq!(
        fixture
            .owner(reqwest::Method::DELETE, "/v1/account/session")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    fixture
        .system
        .store()
        .append_event(&actor, event("never-visible"))
        .unwrap();
    let chunk = tokio::time::timeout(Duration::from_secs(6), response.chunk())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let text = String::from_utf8_lossy(&chunk);
    assert!(
        text.contains("access_revoked") && !text.contains("never-visible"),
        "{text}"
    );
}

#[tokio::test]
async fn owner_stream_rechecks_rotated_credentials_and_unconfigured_service_stays_available() {
    let fixture = Fixture::new().await;
    let cookie = fixture.login_cookie().await;
    let after = fixture.system.store().event_cursor().unwrap() - 1;
    let mut stream = fixture
        .client
        .get(format!(
            "{}/v1/event-stream?after={after}&limit=1",
            fixture.url
        ))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(stream.status(), StatusCode::OK);
    stream.chunk().await.unwrap();
    fixture
        .change_password("updated isolated fixture password")
        .await;
    let chunk = tokio::time::timeout(Duration::from_secs(6), stream.chunk())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(String::from_utf8_lossy(&chunk).contains("access_revoked"));
    let unconfigured = Fixture::with_config(Config::default()).await;
    assert_eq!(
        unconfigured
            .client
            .get(format!("{}/v1/health", unconfigured.url))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        unconfigured
            .owner(reqwest::Method::GET, "/v1/actors/root")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        unconfigured
            .owner(reqwest::Method::POST, "/v1/actors/root/messages")
            .json(&json!({"content": "hello", "attachments": []}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
}

#[tokio::test]
async fn shutdown_closes_actor_and_owner_streams_without_waiting_for_heartbeat() {
    let fixture = Fixture::new().await;
    let after = fixture.system.store().event_cursor().unwrap() - 1;
    let mut streams = Vec::new();
    for path in ["/v1/event-stream", "/v1/actors/root/event-stream"] {
        let mut stream = fixture
            .owner(
                reqwest::Method::GET,
                &format!("{path}?after={after}&limit=1"),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(stream.status(), StatusCode::OK);
        assert!(stream.chunk().await.unwrap().is_some());
        streams.push(stream);
    }
    fixture.system.shutdown().await;
    for mut stream in streams {
        tokio::time::timeout(Duration::from_secs(1), async {
            while stream.chunk().await.unwrap().is_some() {}
        })
        .await
        .expect("shutdown must wake and finish the persistent stream");
    }
}

#[tokio::test]
async fn explicit_report_streams_and_can_be_fetched_before_task_completion() {
    let fixture = Fixture::new().await;
    let actor = fixture.child("reporting").await;
    let input = fixture
        .system
        .send_user(&actor, "Investigate".into(), vec![], None)
        .unwrap();
    fixture.system.store().claim_next(&actor).unwrap().unwrap();
    let cursor = fixture.system.store().event_cursor().unwrap();
    let delivery = fixture
        .system
        .store()
        .deliver(&actor, &input.id, "progress", "Investigation is underway.")
        .unwrap();
    assert_eq!(
        fixture.system.store().message(&input.id).unwrap().status,
        daat_locus::actors::MessageStatus::Running
    );
    let mut response = fixture
        .owner(
            reqwest::Method::GET,
            &format!("/v1/actors/{actor}/event-stream?after={cursor}&limit=10"),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let chunk = tokio::time::timeout(Duration::from_secs(2), response.chunk())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let text = String::from_utf8_lossy(&chunk);
    assert!(
        text.contains("message_added") && text.contains(&delivery.id),
        "{text}"
    );
    let response = fixture
        .owner(
            reqwest::Method::GET,
            &format!("/v1/actors/{actor}/messages/{}", delivery.id),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let data: Value = response.json().await.unwrap();
    assert_eq!(data["kind"], "delivery");
    assert_eq!(data["reply"], "Investigation is underway.");
    let denied = fixture
        .client
        .get(format!(
            "{}/v1/actors/{actor}/messages/{}",
            fixture.url, delivery.id
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
}
