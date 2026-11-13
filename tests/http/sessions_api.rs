use crate::support::Fixture;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

#[tokio::test]
async fn local_owner_cli_lists_sends_and_reads_but_cannot_message_workers_or_archived_sessions() {
    use daat_locus::daemon::control::{self, Action, LaunchOptions, Server};
    let fixture = Fixture::new().await;
    let root = fixture._temp.path().canonicalize().unwrap();
    let state = daat_locus::service::ServiceState::new(fixture.system.clone(), fixture.url.clone())
        .unwrap();
    let server = Server::bind(
        root.clone(),
        root.clone(),
        LaunchOptions {
            profile: None,
            listen: "127.0.0.1:0".parse().unwrap(),
            workspace: root.join("workspace"),
        },
        fixture.url.clone(),
        Some(state),
    )
    .await
    .unwrap();
    let page = control::session_command(
        &root,
        Action::SessionsList {
            archived: false,
            limit: 1,
            after: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(page["items"][0]["id"], "core");
    let sent = control::session_command(
        &root,
        Action::SessionSend {
            session: "core".into(),
            content: "CLI input\n保留原文".into(),
        },
    )
    .await
    .unwrap();
    let stored = fixture
        .system
        .store()
        .message(sent["id"].as_str().unwrap())
        .unwrap();
    assert_eq!(stored.content, "CLI input\n保留原文");
    assert_eq!(stored.source, daat_locus::actors::MessageSource::User);
    let page = control::session_command(
        &root,
        Action::SessionMessages {
            actor: "core".into(),
            limit: 10,
            before: None,
        },
    )
    .await
    .unwrap();
    assert!(
        page["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| message["id"] == sent["id"]
                && message["content"] == "CLI input\n保留原文")
    );
    assert!(
        control::session_command(
            &root,
            Action::SessionSend {
                session: "root".into(),
                content: "Do not send".into()
            }
        )
        .await
        .is_err()
    );
    fixture.system.archive_session("core", true).await.unwrap();
    assert!(
        control::session_command(
            &root,
            Action::SessionSend {
                session: "core".into(),
                content: "Do not send".into()
            }
        )
        .await
        .is_err()
    );
    let forged = fixture
        .client
        .get(format!("{}/v1/sessions", fixture.url))
        .header("x-daat-native-owner", "1")
        .header("x-daat-control-owner", "1")
        .send()
        .await
        .unwrap();
    assert_eq!(forged.status(), StatusCode::UNAUTHORIZED);
    drop(server);
    fixture.system.shutdown().await;
}

#[tokio::test]
async fn sessions_are_created_idempotently_renamed_archived_and_deleted() {
    let fixture = Fixture::new().await;
    assert_eq!(
        fixture
            .owner(Method::POST, "/v1/sessions")
            .json(&json!({}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let create = || {
        fixture
            .owner(Method::POST, "/v1/sessions")
            .header("idempotency-key", "session-lifecycle")
            .json(&json!({}))
    };
    let response = create().send().await.unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let session: Value = response.json().await.unwrap();
    let id = session["id"].as_str().unwrap();
    assert_eq!(session["title"], Value::Null);
    assert_eq!(
        create()
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap()["id"],
        id
    );
    let response = fixture
        .owner(Method::DELETE, &format!("/v1/sessions/{id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(
        fixture
            .owner(Method::POST, &format!("/v1/sessions/{id}/messages"))
            .json(&json!({"content":"No retry key","attachments":[]}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let input = fixture
        .owner(Method::POST, &format!("/v1/sessions/{id}/messages"))
        .header("idempotency-key", "first-input")
        .json(&json!({"content":"First line\nRest of input","attachments":[]}))
        .send()
        .await
        .unwrap();
    assert_eq!(input.status(), StatusCode::ACCEPTED);
    let message: Value = input.json().await.unwrap();
    assert_eq!(message["actor_id"], id);
    let session: Value = fixture
        .owner(Method::GET, &format!("/v1/sessions/{id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(session["title"], "First line");
    assert_eq!(session["status"], "running");
    let renamed: Value = fixture
        .owner(Method::PATCH, &format!("/v1/sessions/{id}"))
        .json(&json!({"title":"  renamed  ","model":null}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(renamed["title"], "renamed");
    let archived = fixture
        .owner(Method::POST, &format!("/v1/sessions/{id}/archive"))
        .send()
        .await
        .unwrap();
    assert_eq!(archived.status(), StatusCode::OK);
    assert_eq!(
        archived.json::<Value>().await.unwrap()["status"],
        "archived"
    );
    assert_eq!(
        fixture
            .owner(Method::POST, &format!("/v1/sessions/{id}/messages"))
            .json(&json!({"content":"rejected","attachments":[]}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    let unarchived = fixture
        .owner(Method::POST, &format!("/v1/sessions/{id}/unarchive"))
        .send()
        .await
        .unwrap();
    assert_eq!(unarchived.json::<Value>().await.unwrap()["status"], "idle");
    let original = fixture
        .system
        .store()
        .message(message["id"].as_str().unwrap())
        .unwrap();
    assert_eq!(
        original.status,
        daat_locus::actors::MessageStatus::Cancelled
    );
    fixture
        .owner(Method::POST, &format!("/v1/sessions/{id}/archive"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    assert_eq!(
        fixture
            .owner(Method::DELETE, &format!("/v1/sessions/{id}"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        fixture
            .owner(Method::GET, &format!("/v1/actors/{id}"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    let events: Value = fixture
        .owner(Method::GET, "/v1/events?after=0&limit=100")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        events["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["event"]["type"] == "session_deleted" && e["event"]["session_id"] == id)
    );
    fixture.system.shutdown().await;
}

#[tokio::test]
async fn owner_cannot_send_or_upload_to_a_worker_and_stage_status_has_no_global_core() {
    let fixture = Fixture::new().await;
    let actor: Value = fixture
        .owner(Method::GET, "/v1/actors/root")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(actor["role"], "worker");
    assert_eq!(actor["session_id"], "core");
    assert!(actor.get("access").is_none());
    for suffix in ["messages", "attachments"] {
        assert_eq!(
            fixture
                .owner(Method::POST, &format!("/v1/sessions/root/{suffix}"))
                .json(&json!({"content":"not allowed","attachments":[]}))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
    }
    let status: Value = fixture
        .owner(Method::GET, "/v1/stage/status")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status["model_ready"], true);
    assert!(status.get("core").is_none());
    fixture.system.shutdown().await;
}

#[tokio::test]
async fn remote_session_routes_return_device_unreachable_for_a_trusted_offline_device() {
    let fixture = Fixture::new().await;
    let target =
        Fixture::with_config_and_account(daat_locus::config::Config::default(), false).await;
    let join = target.stage.host.initialize(false).unwrap();
    let enrollment =
        crate::support::fixture_enrollment(&fixture.stage.host, &join, true, Default::default())
            .unwrap();
    target
        .stage
        .host
        .join(enrollment, &fixture.stage.host.account_id().unwrap())
        .unwrap();
    let stage = target.stage.host.local;
    for suffix in ["sessions", "sessions/missing", "events", "event-stream"] {
        let response = fixture
            .owner(Method::GET, &format!("/v1/stages/{stage}/{suffix}"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            response.json::<Value>().await.unwrap()["code"],
            "device_unreachable"
        );
    }
    fixture.system.shutdown().await;
    target.system.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn remote_session_creation_and_catalog_stream_stay_on_the_target_device() {
    use std::time::Duration;
    let relay = crate::support::relay::Relay::new().await;
    let source = Fixture::new().await;
    let target = Fixture::with_config_and_account(source.system.config(), false).await;
    let join = target.stage.host.initialize(false).unwrap();
    target
        .stage
        .host
        .join(
            crate::support::fixture_enrollment(
                &source.stage.host,
                &join,
                false,
                Default::default(),
            )
            .unwrap(),
            &source.stage.host.account_id().unwrap(),
        )
        .unwrap();
    daat_locus::stage::ledger::StageLedger::attach(target.system.store(), target.stage.host.local)
        .import_peer(
            source.stage.host.enrollment().unwrap(),
            Default::default(),
            chrono::Utc::now().timestamp_millis(),
        )
        .unwrap();
    for fixture in [&source, &target] {
        let mut settings = fixture.stage.host.settings();
        settings.mode = daat_locus::stage::host::Connectivity::Relay;
        settings.relay_origin = Some(relay.endpoint.clone());
        fixture.stage.host.save_settings(settings).unwrap();
        fixture.stage.start().await.unwrap();
    }
    crate::support::relay::wait(|| source.stage.handle(target.stage.host.local).is_ok())
        .await
        .unwrap();
    let stage = target.stage.host.local;
    let response = source
        .owner(Method::POST, &format!("/v1/stages/{stage}/sessions"))
        .header("idempotency-key", "remote-session")
        .json(&json!({"title":"remote"}))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let text = response.text().await.unwrap();
    assert_eq!(status, StatusCode::CREATED, "{text}");
    let session: Value = serde_json::from_str(&text).unwrap();
    let id = session["id"].as_str().unwrap();
    assert_eq!(
        session["conversation_url"],
        format!("/stages/{stage}/actors/{id}")
    );
    assert!(target.system.actor(id).is_ok());
    assert!(source.system.actor(id).is_err());
    let attachment = source
        .owner(
            Method::POST,
            &format!("/v1/stages/{stage}/sessions/{id}/attachments"),
        )
        .header("x-file-name", "remote-note.txt")
        .header("x-file-name-encoding", "utf-8-percent")
        .header("content-type", "text/plain")
        .body("evidence")
        .send()
        .await
        .unwrap();
    assert_eq!(attachment.status(), StatusCode::CREATED);
    let attachment: Value = attachment.json().await.unwrap();
    assert_eq!(
        attachment["uri"],
        format!(
            "/v1/stages/{stage}/actors/{id}/resources/{}",
            attachment["id"].as_str().unwrap()
        )
    );
    let sent = source
        .owner(
            Method::POST,
            &format!("/v1/stages/{stage}/sessions/{id}/messages"),
        )
        .header("idempotency-key", "remote-first-input")
        .json(&json!({"content":"Remote work","attachments":[attachment["id"]]}))
        .send()
        .await
        .unwrap();
    assert_eq!(sent.status(), StatusCode::ACCEPTED);
    let sent: Value = sent.json().await.unwrap();
    assert_eq!(sent["actor_id"], id);
    assert_eq!(
        target
            .system
            .store()
            .message(sent["id"].as_str().unwrap())
            .unwrap()
            .content,
        "Remote work"
    );
    let response = source
        .owner(
            Method::GET,
            &format!("/v1/stages/{stage}/event-stream?after=0"),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut stream = response.bytes_stream();
    use futures_util::StreamExt;
    let activity = tokio::time::timeout(Duration::from_secs(10), async {
        let mut text = String::new();
        while let Some(chunk) = stream.next().await {
            text.push_str(&String::from_utf8_lossy(&chunk.unwrap()));
            if text.contains("session_changed") && text.contains(id) {
                break;
            }
        }
        text
    })
    .await
    .unwrap();
    assert!(activity.contains("event: activity"));
    assert!(activity.contains("session_changed"));
    let archived = source
        .owner(
            Method::POST,
            &format!("/v1/stages/{stage}/sessions/{id}/archive"),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(archived.status(), StatusCode::OK);
    assert_eq!(
        archived.json::<Value>().await.unwrap()["status"],
        "archived"
    );
    assert_eq!(
        source
            .owner(Method::DELETE, &format!("/v1/stages/{stage}/sessions/{id}"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    assert!(target.system.actor(id).is_err());
    let actors_before = target.system.store().actors().unwrap().len();
    let local = target.stage.host.local;
    daat_locus::stage::ledger::StageLedger::attach(target.system.store(), local)
        .import_peer(
            source.stage.host.enrollment().unwrap(),
            daat_locus::stage::ledger::DeviceAuthority {
                read: true,
                converse: true,
                delegate: true,
                read_only: true,
            },
            chrono::Utc::now().timestamp_millis(),
        )
        .unwrap();
    let restricted = source
        .owner(Method::POST, &format!("/v1/stages/{stage}/sessions"))
        .header("idempotency-key", "denied-readonly")
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert!(
        [StatusCode::FORBIDDEN, StatusCode::SERVICE_UNAVAILABLE].contains(&restricted.status())
    );
    assert_eq!(
        target.system.store().actors().unwrap().len(),
        actors_before,
        "Read-only remote authority never creates another session"
    );
    source.stage.stop().await;
    target.stage.stop().await;
    source.system.shutdown().await;
    target.system.shutdown().await;
}
