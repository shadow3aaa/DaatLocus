//! Explicit account reset removes work, never configuration or user files.
use crate::support::Fixture;
use daat_locus::actors::ApprovalMode;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

#[tokio::test]
async fn account_reset_removes_actors_approvals_and_resources_but_preserves_identity_and_configuration()
 {
    let fixture = Fixture::new().await;
    let _core = daat_locus::stage::shared_runtime::Controller::new(&fixture.stage).unwrap();
    let queued = fixture
        .system
        .send_user("core", "discard this context".into(), vec![], None)
        .unwrap();
    let resource = daat_locus::resources::publish(
        &fixture.system,
        "core",
        "old.txt",
        "text/plain",
        b"old core attachment",
    )
    .unwrap();
    let actor = fixture.child("discarded-project").await;
    let remote = daat_locus::stage::identity::DeviceIdentity::generate().unwrap();
    crate::support::fixture_enrollment(
        &fixture.stage.host,
        &remote
            .join_request(chrono::Utc::now().timestamp_millis())
            .unwrap(),
        true,
        vec![daat_locus::stage::ledger::ActorGrant {
            actor_id: actor.clone(),
            read: true,
            converse: true,
            delegate: true,
            read_only: false,
        }],
    )
    .unwrap();
    let approval = fixture.approval(&actor, ApprovalMode::Ask).await;
    let workspace = fixture.system.actor(&actor).unwrap().config.workspace;
    let user_file = workspace.join("keep.txt");
    std::fs::write(&user_file, "user project contents").unwrap();
    let attachment = fixture
        .system
        .actor_data_dir(&actor)
        .join("private-attachment.txt");
    std::fs::write(&attachment, "old attachment").unwrap();
    let app_state = fixture.system.actor_state_dir(&actor).join("state.json");
    std::fs::write(&app_state, "old app state").unwrap();
    let config = serde_json::to_value(fixture.system.config()).unwrap();
    let credentials = fixture.auth.current().unwrap().unwrap().binding();
    let device = fixture.stage.host.profile().unwrap();
    let settings = serde_json::to_value(fixture.stage.host.settings()).unwrap();
    let info: Value = fixture
        .owner(Method::GET, "/v1/account/reset")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let request =
        json!({"account_id": info["account_id"], "expected_generation": info["generation"]});
    let response = fixture
        .owner(Method::POST, "/v1/account/reset")
        .json(&request)
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::NO_CONTENT,
        "{}",
        response.text().await.unwrap()
    );
    assert!(fixture.system.actor(&actor).is_err());
    assert!(fixture.system.store().approval(&approval.id).is_err());
    assert_eq!(fixture.system.store().actors().unwrap().len(), 1);
    let root = fixture.system.actor("core").unwrap();
    assert_eq!(root.config.permissions.approval_mode, ApprovalMode::Auto);
    assert!(root.config.permissions.message_targets.is_empty());
    assert_eq!(
        fixture.system.actor("core").unwrap().config.apps,
        ["actors"]
    );
    assert!(
        fixture
            .system
            .store()
            .history("core", 100)
            .unwrap()
            .is_empty()
    );
    assert!(fixture.system.store().message(&queued.id).is_err());
    assert!(daat_locus::resources::export(&fixture.system, "core", &resource.id).is_err());
    assert_eq!(
        fixture
            .owner(
                Method::GET,
                &format!("/v1/actors/core/resources/{}", resource.id)
            )
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    assert!(!attachment.exists());
    assert!(!app_state.exists());
    assert_eq!(
        std::fs::read_to_string(user_file).unwrap(),
        "user project contents"
    );
    assert_eq!(
        serde_json::to_value(fixture.system.config()).unwrap(),
        config
    );
    assert_eq!(
        fixture.auth.current().unwrap().unwrap().binding(),
        credentials
    );
    assert_eq!(fixture.stage.host.profile().unwrap(), device);
    let peer = daat_locus::stage::ledger::StageLedger::attach(
        fixture.system.store(),
        fixture.stage.host.local,
    )
    .peer(
        remote.profile().stage_id,
        chrono::Utc::now().timestamp_millis(),
    )
    .unwrap();
    assert_eq!(peer.device, remote.profile());
    assert!(
        peer.grants.is_empty(),
        "reset must remove old per-Actor device grants"
    );
    assert_eq!(
        serde_json::to_value(fixture.stage.host.settings()).unwrap(),
        settings
    );
    // Existing Owner sessions stay valid; a stale form cannot reset twice.
    let after: Value = fixture
        .owner(Method::GET, "/v1/account/reset")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_ne!(after["generation"], Value::Null);
    assert_eq!(
        fixture
            .owner(Method::POST, "/v1/account/reset")
            .json(&request)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn account_reset_requires_owner_trusted_origin_and_exact_account_confirmation() {
    let fixture = Fixture::new().await;
    let actor = fixture.child("must-survive-rejected-reset").await;
    let info: Value = fixture
        .owner(Method::GET, "/v1/account/reset")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let request = json!({"account_id": info["account_id"], "expected_generation": null});
    let cookie = fixture.cookie.read().clone();
    assert_eq!(
        fixture
            .client
            .post(format!("{}/v1/account/reset", fixture.url))
            .json(&request)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    for origin in [None, Some("https://untrusted.invalid")] {
        let mut call = fixture
            .client
            .post(format!("{}/v1/account/reset", fixture.url))
            .header("cookie", &cookie)
            .header("x-daat-request", "1");
        if let Some(origin) = origin {
            call = call.header("origin", origin);
        }
        assert_eq!(
            call.json(&request).send().await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        fixture
            .client
            .post(format!("{}/v1/account/reset", fixture.url))
            .header("cookie", &cookie)
            .header("origin", &fixture.url)
            .json(&request)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        fixture
            .owner(Method::POST, "/v1/account/reset")
            .json(&json!({"account_id": "different-account", "expected_generation": null}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert!(fixture.system.actor(&actor).is_ok());
}
