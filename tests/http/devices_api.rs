//! Devices HTTP contracts.
use crate::support::Fixture;
use crate::support::fixture_enrollment;
use daat_locus::actors::ApprovalDecision;
use daat_locus::actors::ApprovalMode;
use daat_locus::config::Config;
use daat_locus::config::ProviderConfig;
use reqwest::StatusCode;
use serde_json::{Value, json};
use std::time::Duration;

#[tokio::test]
async fn account_model_definitions_sync_privately_without_replacing_device_preferences() {
    use daat_locus::stage::ledger::StageLedger;
    let a = Fixture::new().await;
    let mut local = Config::default();
    local.daemon.port = 45990;
    let b = Fixture::with_config_and_account(local, false).await;
    let request = b.stage.host.initialize(false).unwrap();
    b.stage
        .host
        .join(
            fixture_enrollment(&a.stage.host, &request, true, vec![]).unwrap(),
            &a.stage.host.account_id().unwrap(),
        )
        .unwrap();
    StageLedger::attach(b.system.store(), b.stage.host.local)
        .import_peer(
            a.stage.host.enrollment().unwrap(),
            vec![],
            chrono::Utc::now().timestamp_millis(),
        )
        .unwrap();
    for fixture in [&a, &b] {
        let mut settings = fixture.stage.host.settings();

        settings.listen = "127.0.0.1:0".into();
        fixture.stage.host.save_settings(settings).unwrap();
        fixture.stage.start().await.unwrap();
    }
    let address = a.stage.address.read().unwrap();
    b.stage
        .dial_tcp(a.stage.host.local, address, None)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !b.system.config().models.contains_key("mock") {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while b.auth.current().unwrap().is_none() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    b.login_cookie().await;
    let readiness: Value = b
        .owner(reqwest::Method::GET, "/v1/config/readiness")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        readiness["ready"], true,
        "synced configuration must immediately drive readiness"
    );
    let revision = b
        .stage
        .host
        .configurations
        .revision(&b.stage.host)
        .unwrap()
        .unwrap();
    assert_eq!(b.system.config().daemon.port, 45990);
    let mut next = a.system.config();
    let model = next.models["mock"].clone();
    next.models.insert("account-model".into(), model);
    if let ProviderConfig::OpenaiCompatible { api_key, .. } =
        next.providers.get_mut("mock").unwrap()
    {
        *api_key = "synchronized-private-key".into();
    }
    let persistent = next.clone();
    let host = a.stage.host.clone();
    a.system
        .update_config_with(next, async move {
            host.configurations.publish(&host, &persistent)?;
            Ok(())
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !b.system.config().models.contains_key("account-model") {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(b.system.config().daemon.port, 45990);
    assert!(
        matches!(&b.system.config().providers["mock"],ProviderConfig::OpenaiCompatible {api_key,..} if api_key=="synchronized-private-key")
    );
    let next_revision = b
        .stage
        .host
        .configurations
        .revision(&b.stage.host)
        .unwrap()
        .unwrap();
    assert_ne!(
        revision, next_revision,
        "a new synced model must trigger client model refresh even while readiness remains true"
    );
    assert!(!next_revision.contains("synchronized-private-key"));
    let config: Value = b
        .owner(reqwest::Method::GET, "/v1/config")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        config["models"]
            .as_array()
            .unwrap()
            .iter()
            .any(|model| model["name"] == "account-model")
    );
    let record = a.stage.host.configurations.current().unwrap().unwrap();
    assert!(!format!("{record:?}").contains("synchronized-private-key"));
    let foreign = Fixture::new().await;
    assert!(
        foreign
            .stage
            .host
            .configurations
            .needs_import(&foreign.stage.host, &record)
            .is_err()
    );
    let child = b.child("kept-after-provider-removal").await;
    let mut preference = b.system.actor(&child).unwrap().config;
    preference.model = Some("account-model".into());
    b.system.update_owned(&child, preference).await.unwrap();
    let mut empty = a.system.config();
    empty.providers.clear();
    empty.models.clear();
    empty.main_model.clear();
    empty.approval_model = None;
    let persistent = empty.clone();
    let host = a.stage.host.clone();
    a.system
        .update_config_with(empty, async move {
            host.configurations.publish(&host, &persistent)?;
            Ok(())
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !b.system.config().models.is_empty() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("removing the last provider must synchronize too");
    assert_eq!(b.system.config().daemon.port, 45990);
    assert!(b.system.actor(&child).unwrap().config.model.is_none());
    assert!(b.system.config().validate().is_err());
    a.stage.stop().await;
    b.stage.stop().await;
    a.system.shutdown().await;
    b.system.shutdown().await;
    foreign.system.shutdown().await;
}

#[tokio::test]
async fn a_paired_device_without_enrollment_authority_cannot_import_login_credentials() {
    use daat_locus::stage::{ledger::StageLedger, protocol::Body};
    let a = Fixture::new().await;
    let b = Fixture::with_config_and_account(a.system.config(), false).await;
    let request = b.stage.host.initialize(false).unwrap();
    let enrollment = fixture_enrollment(&a.stage.host, &request, false, vec![]).unwrap();
    b.stage
        .host
        .join(enrollment, &a.stage.host.account_id().unwrap())
        .unwrap();
    StageLedger::attach(b.system.store(), b.stage.host.local)
        .import_peer(
            a.stage.host.enrollment().unwrap(),
            vec![],
            chrono::Utc::now().timestamp_millis(),
        )
        .unwrap();
    for fixture in [&a, &b] {
        let mut settings = fixture.stage.host.settings();

        settings.listen = "127.0.0.1:0".into();
        fixture.stage.host.save_settings(settings).unwrap();
        fixture.stage.start().await.unwrap();
    }
    let address = a.stage.address.read().unwrap();
    b.stage
        .dial_tcp(a.stage.host.local, address, None)
        .await
        .unwrap();
    let peer = b.stage.handle(a.stage.host.local).unwrap();
    let denied = peer
        .rpc(Body::AccountConfigurationQuery {
            known_binding: None,
        })
        .await;
    assert!(
        matches!(denied, Err(_) | Ok(Body::Error { .. })),
        "conversation membership cannot read model credentials"
    );
    assert!(b.stage.host.configurations.current().unwrap().is_none());
    let denied = peer
        .rpc(Body::CoordinationRequest {
            request: Box::new(daat_locus::stage::coordination::Request::Command {
                operation: daat_locus::stage::shared::Operation::Inspect,
                receipts: vec![],
                instance: uuid::Uuid::new_v4(),
            }),
        })
        .await;
    assert!(
        matches!(denied, Err(_) | Ok(Body::Error { .. })),
        "conversation membership cannot control execution metadata"
    );
    let response = tokio::time::timeout(
        Duration::from_secs(3),
        peer.rpc(Body::AccountCredentialsQuery {
            known_binding: None,
        }),
    )
    .await
    .unwrap();
    assert!(
        matches!(response, Err(_) | Ok(Body::Error { .. })),
        "private credentials must not be returned"
    );
    assert!(b.auth.current().unwrap().is_none());
    b.stage.stop().await;
    a.stage.stop().await;
    b.system.shutdown().await;
    a.system.shutdown().await;
}

#[tokio::test]
async fn remote_actor_http_transfers_pathless_attachments_and_enforces_source_account_grants() {
    use daat_locus::stage::ledger::StageLedger;
    let a = Fixture::new().await;
    let b = Fixture::with_config_and_account(a.system.config(), false).await;
    a.stage.host.initialize(true).unwrap();
    let request = b.stage.host.initialize(false).unwrap();
    let target = a.child("remote-source").await;
    let secret = a.child("private-source").await;
    // A recorded foreign account is never adopted during legacy-tree migration.
    rusqlite::Connection::open(a._temp.path().join("actors/actors.sqlite3"))
        .unwrap()
        .execute(
            "INSERT INTO stage_account_actors VALUES(?1,'another-account')",
            [&secret],
        )
        .unwrap();
    let enrollment = fixture_enrollment(&a.stage.host, &request, true, vec![]).unwrap();
    b.stage
        .host
        .join(enrollment, &a.stage.host.account_id().unwrap())
        .unwrap();
    StageLedger::attach(b.system.store(), b.stage.host.local)
        .import_peer(
            a.stage.host.enrollment().unwrap(),
            vec![],
            chrono::Utc::now().timestamp_millis(),
        )
        .unwrap();
    for fixture in [&a, &b] {
        let mut settings = fixture.stage.host.settings();

        settings.listen = "127.0.0.1:0".into();
        fixture.stage.host.save_settings(settings).unwrap();
        fixture.stage.start().await.unwrap();
    }
    let address = a.stage.address.read().unwrap();
    b.stage
        .dial_tcp(a.stage.host.local, address, None)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while b.auth.current().unwrap().is_none() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    b.login_cookie().await;
    let path = format!("/v1/stages/{}/actors/{target}", a.stage.host.local);
    let detail = b.owner(reqwest::Method::GET, &path).send().await.unwrap();
    assert_eq!(detail.status(), StatusCode::OK);
    let detail: Value = detail.json().await.unwrap();
    assert_eq!(detail["access"]["manage"], true);
    assert_eq!(detail["model_ready"], true);
    assert_eq!(
        detail["conversation_url"],
        format!("/stages/{}/actors/{target}", a.stage.host.local)
    );
    // Actors created through the Core hierarchy need no per-device grants either.
    let hosted = a
        .system
        .create_owned("core", a.system.actor(&target).unwrap().config)
        .unwrap();
    let nested = a
        .system
        .create_owned(&hosted.id, hosted.config.clone())
        .unwrap();
    for id in [&hosted.id, &nested.id] {
        let response = b
            .owner(
                reqwest::Method::GET,
                &format!("/v1/stages/{}/actors/{id}/messages", a.stage.host.local),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
    let denied = format!("/v1/stages/{}/actors/{secret}", a.stage.host.local);
    assert_eq!(
        b.owner(reqwest::Method::GET, &denied)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        b.owner(reqwest::Method::PATCH, &path)
            .json(&json!({"approval_mode":"none"}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        b.owner(reqwest::Method::PUT, &path)
            .json(&json!({"permissions":{"read_only":false}}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::METHOD_NOT_ALLOWED
    );
    assert_eq!(
        b.owner(reqwest::Method::PATCH, &path)
            .json(&json!({"approval_mode":"none","permissions":{"read_only":false}}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        b.owner(reqwest::Method::DELETE, &path)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::METHOD_NOT_ALLOWED
    );
    assert!(a.system.actor(&target).is_ok());
    let upload = b
        .owner(reqwest::Method::POST, &format!("{path}/attachments"))
        .header("x-file-name", "remote.txt")
        .header("content-type", "text/plain")
        .body("remote attachment")
        .send()
        .await
        .unwrap();
    assert_eq!(
        upload.status(),
        StatusCode::CREATED,
        "{}",
        upload.text().await.unwrap()
    );
    let upload = upload.json::<Value>().await.unwrap();
    let id = upload["id"].as_str().unwrap();
    let sent = b
        .owner(reqwest::Method::POST, &format!("{path}/messages"))
        .header("idempotency-key", "remote-with-attachment")
        .json(&json!({"content":"read the attachment","attachments":[id]}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        sent.status(),
        StatusCode::OK,
        "{}",
        sent.text().await.unwrap()
    );
    let sent = sent.json::<Value>().await.unwrap();
    assert_eq!(sent["attachments"][0]["uri"], upload["uri"]);
    let local = a
        .system
        .store()
        .message(sent["id"].as_str().unwrap())
        .unwrap();
    assert_eq!(local.attachments.len(), 1);
    let download = b
        .owner(reqwest::Method::GET, upload["uri"].as_str().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(download.status(), StatusCode::OK);
    assert_eq!(download.text().await.unwrap(), "remote attachment");
    let changed = b
        .owner(reqwest::Method::PATCH, &path)
        .json(&json!({"model":null}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        changed.status(),
        StatusCode::OK,
        "{}",
        changed.text().await.unwrap()
    );
    assert!(changed.json::<Value>().await.unwrap()["config"]["model"].is_null());
    let approval = a.approval(&target, ApprovalMode::Ask).await;
    let remote_id = format!("{}~{}", a.stage.host.local, approval.id);
    let pending = b
        .owner(
            reqwest::Method::GET,
            "/v1/approvals?awaiting_owner=true&limit=1",
        )
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(pending["items"][0]["id"], remote_id);
    let endpoint = format!("/v1/approvals/{remote_id}");
    assert_eq!(
        b.owner(reqwest::Method::POST, &endpoint)
            .json(&json!({"decision":"approve","arguments":{"path":"changed"}}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let resolved = b
        .owner(reqwest::Method::POST, &endpoint)
        .json(&json!({"decision":"approve"}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resolved.status(),
        StatusCode::OK,
        "{}",
        resolved.text().await.unwrap()
    );
    assert_eq!(
        a.system.store().approval(&approval.id).unwrap().decision,
        ApprovalDecision::Approved
    );
    assert_eq!(
        a.system
            .store()
            .approval(&approval.id)
            .unwrap()
            .call
            .arguments,
        approval.call.arguments
    );
    a.stage
        .host
        .revoke_account_device(b.stage.host.local)
        .unwrap();
    let revoked = b.owner(reqwest::Method::GET, &path).send().await.unwrap();
    assert!(matches!(
        revoked.status(),
        StatusCode::FORBIDDEN | StatusCode::SERVICE_UNAVAILABLE
    ));
    a.stage.stop().await;
    b.stage.stop().await;
    a.system.shutdown().await;
    b.system.shutdown().await;
}

#[tokio::test]
async fn recovery_record_queries_require_owner_access_and_bounded_cursors() {
    let fixture = Fixture::new().await;
    fixture.stage.start().await.unwrap();
    let path = "/v1/stage/recovery/archives?limit=30";
    assert_eq!(
        fixture
            .client
            .get(format!("{}{}", fixture.url, path))
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    let response = fixture
        .owner(reqwest::Method::GET, path)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["items"], json!([]));
    assert!(body["next_cursor"].is_null());
    for path in [
        "/v1/stage/recovery/archives?limit=101",
        "/v1/stage/recovery/archives?after=-1",
        "/v1/stage/recovery/archives/not-an-id/messages?limit=30",
    ] {
        assert_eq!(
            fixture
                .owner(reqwest::Method::GET, path)
                .send()
                .await
                .unwrap()
                .status(),
            reqwest::StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    fixture.stage.stop().await;
    fixture.system.shutdown().await;
}
#[tokio::test]
async fn joining_devices_are_replicas_and_offline_replicas_do_not_block_the_account_authority() {
    use daat_locus::stage::{ledger::StageLedger, shared::Operation};
    let desktop = Fixture::new().await;
    let phone = Fixture::with_config_and_account(desktop.system.config(), false).await;
    let request = phone.stage.host.initialize(false).unwrap();
    phone
        .stage
        .host
        .join(
            fixture_enrollment(&desktop.stage.host, &request, true, vec![]).unwrap(),
            &desktop.stage.host.account_id().unwrap(),
        )
        .unwrap();
    StageLedger::attach(phone.system.store(), phone.stage.host.local)
        .import_peer(
            desktop.stage.host.enrollment().unwrap(),
            vec![],
            chrono::Utc::now().timestamp_millis(),
        )
        .unwrap();
    for fixture in [&desktop, &phone] {
        let mut settings = fixture.stage.host.settings();
        settings.listen = "127.0.0.1:0".into();
        fixture.stage.host.save_settings(settings).unwrap();
        fixture.stage.start().await.unwrap();
    }
    let address = desktop.stage.address.read().unwrap();
    phone
        .stage
        .dial_tcp(desktop.stage.host.local, address, None)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if phone
                .stage
                .coordinate(Operation::Inspect, vec![])
                .await
                .is_ok()
                && desktop.stage.voting_member(phone.stage.host.local) == Some(false)
                && phone.stage.voting_member(phone.stage.host.local) == Some(false)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap();
    phone.stage.stop().await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while desktop.stage.state(phone.stage.host.local).status
            == daat_locus::stage::connection::ConnectionStatus::Online
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let devices: Value = desktop
        .owner(reqwest::Method::GET, "/v1/stage/devices?limit=64")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let phone_id = phone.stage.host.local.to_string();
    let peer = devices["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|device| device["stage_id"] == phone_id)
        .unwrap();
    assert_eq!(peer["connection"]["status"], "unreachable");
    let status: Value = desktop
        .owner(reqwest::Method::GET, "/v1/stage/status")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status["core"]["ready"], false);

    assert!(
        desktop
            .stage
            .coordinate(Operation::Inspect, vec![])
            .await
            .is_ok()
    );
    assert_eq!(
        desktop.stage.voting_member(phone.stage.host.local),
        Some(false)
    );
    desktop.stage.stop().await;
    desktop.system.shutdown().await;
    phone.system.shutdown().await;
}

#[tokio::test]
async fn core_readiness_rejects_unsynchronized_input_without_queuing_it() {
    let f = Fixture::new().await;
    let response = f
        .client
        .get(format!("{}/v1/stage/status", f.url))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let status: Value = f
        .owner(reqwest::Method::GET, "/v1/stage/status")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(status["core"].is_null());
    assert_eq!(status.as_object().unwrap().len(), 2);
    assert_eq!(status["public_login"]["state"], "not_configured");
    assert!(status["public_login"]["error_code"].is_null());
    let controller = daat_locus::stage::shared_runtime::Controller::new(&f.stage).unwrap();
    assert!(!controller.accepts_input());

    let response = f
        .owner(reqwest::Method::POST, "/v1/actors/core/messages")
        .json(&json!({"content":"keep this draft","attachments":[]}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let error: Value = response.json().await.unwrap();
    assert!(error.to_string().contains("core_not_ready"));
    let messages: Value = f
        .owner(reqwest::Method::GET, "/v1/actors/core/messages?limit=10")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(messages["items"].as_array().unwrap().is_empty());
    controller.stop().await;
}
