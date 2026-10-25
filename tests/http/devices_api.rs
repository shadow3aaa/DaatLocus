//! Devices HTTP contracts.
use crate::support::Fixture;
use crate::support::fixture_enrollment;
use daat_locus::actors::ApprovalDecision;
use daat_locus::config::Config;
use daat_locus::config::ProviderConfig;
use reqwest::StatusCode;
use serde_json::{Value, json};
use std::time::Duration;

async fn wait_for_account_configuration(
    source: &Fixture,
    target: &Fixture,
    phase: &str,
    ready: impl Fn(&Config) -> bool,
) {
    let expected_revision = source
        .stage
        .host
        .configurations
        .revision(&source.stage.host)
        .expect("source configuration revision must be readable")
        .expect("source must publish its configuration before waiting for synchronization");
    let started = std::time::Instant::now();
    // This checks convergence, not a five-second performance contract. Dialing
    // still leaves reset negotiation and durable configuration import pending.
    let synchronized = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if ready(&target.system.config())
                && target
                    .stage
                    .host
                    .configurations
                    .revision(&target.stage.host)
                    .expect("target configuration revision must be readable")
                    .as_ref()
                    == Some(&expected_revision)
                && target.auth.current().unwrap().is_some()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(
        synchronized.is_ok(),
        "account configuration did not converge during {phase} after {:?}; \
         expected_revision={expected_revision:?}; target_revision={:?}; \
         target_models={:?}; source_credentials_present={:?}; target_credentials_present={:?}; \
         source_link={:?}; source_error={:?}; target_link={:?}; target_error={:?}",
        started.elapsed(),
        target
            .stage
            .host
            .configurations
            .revision(&target.stage.host),
        target.system.config().models.keys().collect::<Vec<_>>(),
        source.auth.current().map(|record| record.is_some()),
        target.auth.current().map(|record| record.is_some()),
        source.stage.state(target.stage.host.local),
        source.stage.last_error(target.stage.host.local),
        target.stage.state(source.stage.host.local),
        target.stage.last_error(source.stage.host.local),
    );
}

// Match the service runtime so one device's synchronous storage does not stall
// both devices' transport and heartbeat tasks.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn account_model_definitions_sync_privately_without_replacing_device_preferences() {
    let relay = crate::support::relay::Relay::new().await;
    use daat_locus::stage::ledger::StageLedger;
    let a = Fixture::new().await;
    let mut local = Config::default();
    local.daemon.port = 45990;
    let b = Fixture::with_config_and_account(local, false).await;
    let request = b.stage.host.initialize(false).unwrap();
    b.stage
        .host
        .join(
            fixture_enrollment(&a.stage.host, &request, true, Default::default()).unwrap(),
            &a.stage.host.account_id().unwrap(),
        )
        .unwrap();
    StageLedger::attach(b.system.store(), b.stage.host.local)
        .import_peer(
            a.stage.host.enrollment().unwrap(),
            Default::default(),
            chrono::Utc::now().timestamp_millis(),
        )
        .unwrap();
    for fixture in [&a, &b] {
        let mut settings = fixture.stage.host.settings();

        settings.mode = daat_locus::stage::host::Connectivity::Relay;
        settings.relay_origin = Some(relay.endpoint.clone());
        fixture.stage.host.save_settings(settings).unwrap();
        fixture.stage.start().await.unwrap();
    }
    crate::support::relay::wait(|| b.stage.handle(a.stage.host.local).is_ok())
        .await
        .unwrap();
    wait_for_account_configuration(&a, &b, "initial model and credential import", |config| {
        config.models.contains_key("mock")
    })
    .await;
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
    wait_for_account_configuration(&a, &b, "model addition", |config| {
        config.models.contains_key("account-model")
    })
    .await;
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
    wait_for_account_configuration(&a, &b, "last provider removal", |config| {
        config.models.is_empty()
    })
    .await;
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
    let relay = crate::support::relay::Relay::new().await;
    use daat_locus::stage::{ledger::StageLedger, protocol::Body};
    let a = Fixture::new().await;
    let b = Fixture::with_config_and_account(a.system.config(), false).await;
    let request = b.stage.host.initialize(false).unwrap();
    let enrollment =
        fixture_enrollment(&a.stage.host, &request, false, Default::default()).unwrap();
    b.stage
        .host
        .join(enrollment, &a.stage.host.account_id().unwrap())
        .unwrap();
    StageLedger::attach(b.system.store(), b.stage.host.local)
        .import_peer(
            a.stage.host.enrollment().unwrap(),
            Default::default(),
            chrono::Utc::now().timestamp_millis(),
        )
        .unwrap();
    for fixture in [&a, &b] {
        let mut settings = fixture.stage.host.settings();

        settings.mode = daat_locus::stage::host::Connectivity::Relay;
        settings.relay_origin = Some(relay.endpoint.clone());
        fixture.stage.host.save_settings(settings).unwrap();
        fixture.stage.start().await.unwrap();
    }
    crate::support::relay::wait(|| b.stage.handle(a.stage.host.local).is_ok())
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
    let relay = crate::support::relay::Relay::new().await;
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
            "INSERT OR REPLACE INTO stage_account_actors VALUES(?1,'another-account')",
            [&secret],
        )
        .unwrap();
    let enrollment = fixture_enrollment(&a.stage.host, &request, true, Default::default()).unwrap();
    b.stage
        .host
        .join(enrollment, &a.stage.host.account_id().unwrap())
        .unwrap();
    StageLedger::attach(b.system.store(), b.stage.host.local)
        .import_peer(
            a.stage.host.enrollment().unwrap(),
            Default::default(),
            chrono::Utc::now().timestamp_millis(),
        )
        .unwrap();
    for fixture in [&a, &b] {
        let mut settings = fixture.stage.host.settings();

        settings.mode = daat_locus::stage::host::Connectivity::Relay;
        settings.relay_origin = Some(relay.endpoint.clone());
        fixture.stage.host.save_settings(settings).unwrap();
        fixture.stage.start().await.unwrap();
    }
    crate::support::relay::wait(|| b.stage.handle(a.stage.host.local).is_ok())
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
        StatusCode::UNPROCESSABLE_ENTITY
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
    let approval = a.approval(&target).await;
    let remote_id = format!("{}~{}", a.stage.host.local, approval.id);
    let endpoint = format!("/v1/approvals/{remote_id}");
    let detail = b
        .owner(reqwest::Method::GET, &endpoint)
        .send()
        .await
        .unwrap();
    assert_eq!(detail.status(), StatusCode::OK);
    let detail: Value = detail.json().await.unwrap();
    assert_eq!(detail["id"], remote_id);
    for decision in ["approve", "deny"] {
        assert_eq!(
            b.owner(reqwest::Method::POST, &endpoint)
                .json(&json!({"decision":decision}))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::METHOD_NOT_ALLOWED
        );
    }
    assert_eq!(
        a.system.store().approval(&approval.id).unwrap().decision,
        ApprovalDecision::Pending
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
    let relay = crate::support::relay::Relay::new().await;
    use daat_locus::stage::{ledger::StageLedger, shared::Operation};
    let desktop = Fixture::new().await;
    let phone = Fixture::with_config_and_account(desktop.system.config(), false).await;
    let request = phone.stage.host.initialize(false).unwrap();
    phone
        .stage
        .host
        .join(
            fixture_enrollment(&desktop.stage.host, &request, true, Default::default()).unwrap(),
            &desktop.stage.host.account_id().unwrap(),
        )
        .unwrap();
    StageLedger::attach(phone.system.store(), phone.stage.host.local)
        .import_peer(
            desktop.stage.host.enrollment().unwrap(),
            Default::default(),
            chrono::Utc::now().timestamp_millis(),
        )
        .unwrap();
    for fixture in [&desktop, &phone] {
        let mut settings = fixture.stage.host.settings();
        settings.mode = daat_locus::stage::host::Connectivity::Relay;
        settings.relay_origin = Some(relay.endpoint.clone());
        fixture.stage.host.save_settings(settings).unwrap();
        fixture.stage.start().await.unwrap();
    }
    crate::support::relay::wait(|| phone.stage.handle(desktop.stage.host.local).is_ok())
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn core_conversation_pages_and_live_updates_do_not_wait_for_execution_replication() {
    use daat_locus::{
        actors::MessageStatus,
        stage::{ledger::StageLedger, shared_store},
    };
    use reqwest::Method;
    let relay = crate::support::relay::Relay::new().await;
    let a = Fixture::new_with_factory(std::sync::Arc::new(CoreViewModel)).await;
    let b = Fixture::with_factory_and_account(
        a.system.config(),
        false,
        std::sync::Arc::new(CoreViewModel),
    )
    .await;
    let mut expected = Vec::new();
    for n in 0..45 {
        let record = a
            .system
            .send_user("core", format!("Archived request {n}"), vec![], None)
            .unwrap();
        a.system
            .store()
            .finish_message(
                &record.id,
                MessageStatus::Completed,
                Some("Archived reply"),
                None,
            )
            .unwrap();
        expected.push(record.id);
    }
    let request = b.stage.host.initialize(false).unwrap();
    let account = a.stage.host.account_id().unwrap();
    b.stage
        .host
        .join(
            fixture_enrollment(&a.stage.host, &request, true, Default::default()).unwrap(),
            &account,
        )
        .unwrap();
    StageLedger::attach(b.system.store(), b.stage.host.local)
        .import_peer(
            a.stage.host.enrollment().unwrap(),
            Default::default(),
            chrono::Utc::now().timestamp_millis(),
        )
        .unwrap();
    shared_store::initialize(b.system.store(), &account).unwrap();
    let database =
        rusqlite::Connection::open(b._temp.path().join("actors/actors.sqlite3")).unwrap();
    database.execute_batch("CREATE TRIGGER hold_execution_replica BEFORE INSERT ON core_rows BEGIN SELECT RAISE(FAIL,'execution checkpoint deliberately held'); END;").unwrap();
    for fixture in [&a, &b] {
        let mut settings = fixture.stage.host.settings();
        settings.mode = daat_locus::stage::host::Connectivity::Relay;
        settings.relay_origin = Some(relay.endpoint.clone());
        fixture.stage.host.save_settings(settings).unwrap();
        fixture.stage.start().await.unwrap();
    }
    a.system.resume_execution().await.unwrap();
    crate::support::relay::wait(|| b.stage.handle(a.stage.host.local).is_ok())
        .await
        .unwrap();
    wait_for_account_configuration(&a, &b, "lazy conversation view", |config| {
        config.models.contains_key("mock")
    })
    .await;
    b.login_cookie().await;
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let status = b.stage.core_status().unwrap();
            if status["ready"] == true && status["synchronized"] == false {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("connected learner must admit forwarded inputs before importing its execution context");
    assert_eq!(
        shared_store::head(b.system.store()).unwrap(),
        daat_locus::stage::shared::Head::default()
    );
    assert!(
        b.system
            .store()
            .messages("core", None, 100)
            .unwrap()
            .items
            .is_empty()
    );
    let unauthorized = b
        .client
        .get(format!("{}/v1/actors/core/messages?limit=5", b.url))
        .send()
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
    let response = b
        .owner(Method::GET, "/v1/actors/core/messages?limit=5")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let page: Value = response.json().await.unwrap();
    let ids: Vec<_> = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(ids, expected[40..].to_vec());
    assert!(
        page["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["actor_id"] == "core")
    );
    let source_view: Value = b
        .owner(
            Method::GET,
            &format!(
                "/v1/stages/{}/actors/core/messages?limit=5",
                a.stage.host.local
            ),
        )
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let qualified_core = format!("{}~core", a.stage.host.local);
    assert!(
        source_view["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["actor_id"] == qualified_core)
    );
    let before = page["next_cursor"]
        .as_i64()
        .expect("older messages remain paginated");
    let response = b
        .owner(
            Method::GET,
            &format!("/v1/actors/core/messages?limit=5&before={before}"),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let older: Value = response.json().await.unwrap();
    let ids: Vec<_> = older["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(ids, expected[35..40].to_vec());
    // A view cursor must not hydrate or certify the execution checkpoint.
    assert!(
        b.system
            .store()
            .messages("core", None, 100)
            .unwrap()
            .items
            .is_empty()
    );
    let mut events = b
        .owner(
            Method::GET,
            &format!(
                "/v1/actors/core/event-stream?after={}",
                page["event_cursor"].as_i64().unwrap()
            ),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(events.status(), StatusCode::OK);
    let original = a.system.store().message(&expected[44]).unwrap();
    let mut next = original.clone();
    next.seq = 0;
    next.id = uuid::Uuid::new_v4().to_string();
    next.content = "A newly published completed message".into();
    next.created_at_ms += 100;
    next.updated_at_ms += 100;
    let next = a.system.store().enqueue(next, None).unwrap();
    let mut text = String::new();
    tokio::time::timeout(Duration::from_secs(15), async {
        while !text.contains(&next.id) {
            let bytes = events
                .chunk()
                .await
                .unwrap()
                .expect("incremental stream ended early");
            text.push_str(std::str::from_utf8(&bytes).unwrap());
        }
    })
    .await
    .expect("latest Core updates must stream without a full execution replica");
    assert!(
        !text.contains(&expected[0]),
        "opening the stream must not replay archived message history"
    );
    assert_eq!(
        shared_store::head(b.system.store()).unwrap(),
        daat_locus::stage::shared::Head::default()
    );
    let incremental: Value = b
        .owner(Method::GET, "/v1/actors/core/events?after=0")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        incremental["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["event"]["message_id"] == next.id)
    );
    assert!(
        incremental["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["actor_id"] == "core")
    );
    drop(events);
    b.system.resume_execution().await.unwrap();
    let submit = || {
        b.owner(Method::POST, "/v1/actors/core/messages")
        .header("idempotency-key", "forward-before-execution-sync")
        .json(&json!({"content":"Forward this without importing execution history", "attachments":[]}))
    };
    let reply = submit().send().await.unwrap();
    assert_eq!(reply.status(), StatusCode::ACCEPTED);
    let sent: Value = reply.json().await.unwrap();
    let retry: Value = submit().send().await.unwrap().json().await.unwrap();
    assert_eq!(
        retry["id"], sent["id"],
        "forwarded retries must retain the durable input identity"
    );
    let sent_id = sent["id"].as_str().unwrap();
    let source = a.system.store().message(sent_id).unwrap();
    assert!(matches!(
        source.source,
        daat_locus::actors::MessageSource::User
    ));
    assert!(source.remote_authority.is_none());
    assert_eq!(
        b.stage.core_status().unwrap()["worker_stage_id"],
        a.stage.host.local.to_string()
    );
    assert_eq!(
        shared_store::head(b.system.store()).unwrap(),
        daat_locus::stage::shared::Head::default()
    );
    let cancelled = b
        .owner(
            Method::POST,
            &format!("/v1/actors/core/messages/{sent_id}/cancel"),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(cancelled.status(), StatusCode::ACCEPTED);
    crate::support::relay::wait(|| {
        a.system.store().message(sent_id).unwrap().status == MessageStatus::Cancelled
    })
    .await
    .unwrap();
    database
        .execute_batch("DROP TRIGGER hold_execution_replica;")
        .unwrap();
    a.stage.stop().await;
    b.stage.stop().await;
    a.system.shutdown().await;
    b.system.shutdown().await;
}

struct CoreViewModel;
impl daat_locus::actors::ModelFactory for CoreViewModel {
    fn build(
        &self,
        _: &str,
        _: &Config,
    ) -> miette::Result<std::sync::Arc<dyn daat_locus::core::ModelProvider>> {
        Ok(std::sync::Arc::new(Self))
    }
}
#[async_trait::async_trait]
impl daat_locus::core::ModelProvider for CoreViewModel {
    async fn complete_json(
        &self,
        _: daat_locus::model_protocol::PromptRequest,
        _: daat_locus::core::ModelRequestOptions,
    ) -> miette::Result<Value> {
        panic!("Core view fixture must not request a review model")
    }
    async fn complete_agent_turn(
        &self,
        _: daat_locus::model_protocol::AgentTurnRequest,
        _: daat_locus::core::ModelRequestOptions,
    ) -> miette::Result<daat_locus::model_protocol::AgentTurnStreamResult> {
        std::future::pending().await
    }
    fn request_budget_limits(&self) -> daat_locus::context_budget::RequestBudgetLimits {
        daat_locus::context_budget::RequestBudgetLimits {
            context_window_tokens: 100_000,
            auto_compact_threshold_tokens: 90_000,
            reserved_output_tokens: 1000,
        }
    }
    fn token_usage_info(&self) -> daat_locus::core::TokenUsageInfo {
        Default::default()
    }
    fn model_name(&self) -> String {
        "core-view-fixture".into()
    }
}
