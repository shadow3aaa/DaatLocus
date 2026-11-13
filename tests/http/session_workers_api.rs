//! Session membership is routed, not replicated execution state.
use crate::support::{Fixture, fixture_enrollment};
use daat_locus::{
    actor_app::ActorApp,
    actors::{ApprovalDecision, ApprovalRecord, MessageStatus},
    admin_actors_app::AdminActorsApp,
    app::App,
    model_protocol::AgentToolCall,
    stage::{host::Connectivity, ledger::StageLedger},
};
use serde_json::{Value, json};

async fn connect(a: &Fixture, b: &Fixture, endpoint: &str) {
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
    for device in [a, b] {
        let mut settings = device.stage.host.settings();
        settings.mode = Connectivity::Relay;
        settings.relay_origin = Some(endpoint.into());
        device.stage.host.save_settings(settings).unwrap();
        device.stage.start().await.unwrap();
    }
    crate::support::relay::wait(|| {
        a.stage.handle(b.stage.host.local).is_ok() && b.stage.handle(a.stage.host.local).is_ok()
    })
    .await
    .unwrap();
}
fn context(f: &Fixture) -> daat_locus::app::AppToolExecutionContext {
    let input = f
        .system
        .send_user("core", "Coordinate my project".into(), vec![], None)
        .unwrap();
    let claimed = f.system.store().claim_next("core").unwrap().unwrap();
    assert_eq!(claimed.id, input.id);
    f.system
        .execution_context(&f.system.actor("core").unwrap(), &claimed)
        .unwrap()
}
fn call(name: &str, arguments: Value) -> AgentToolCall {
    AgentToolCall {
        id: uuid::Uuid::new_v4().to_string(),
        name: name.into(),
        arguments,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn remote_workers_are_session_scoped_and_feed_the_host_catalog() {
    let relay = crate::support::relay::Relay::new().await;
    let a = Fixture::new().await;
    let b = Fixture::with_config_and_account(a.system.config(), false).await;
    connect(&a, &b, &relay.endpoint).await;
    let ctx = context(&a);
    let mut actors = ActorApp::new("core".into(), a.system.clone());
    let request = call(
        "create",
        json!({"stage_id":b.stage.host.local,"name":"Remote researcher","workspace":null,"model":null,"instructions":"Research this project","task":"Inspect the project","forward_input":false}),
    );
    let created = actors.execute_tool(&request, &ctx).await.unwrap();
    let id = created.payload["id"].as_str().unwrap();
    assert!(
        a.system.actor(id).is_err(),
        "The session host must not copy a worker record"
    );
    assert_eq!(b.system.actor(id).unwrap().session_id, "core");
    let bctx = context(&b);
    let mut bapp = ActorApp::new("core".into(), b.system.clone());
    let local = bapp
        .execute_tool(&call("list", json!({"limit":100})), &bctx)
        .await
        .unwrap();
    assert!(
        !local.payload["actors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|actor| actor["id"] == id),
        "Identical legacy core IDs on different hosts are different sessions"
    );
    let claimed = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if let Some(input) = b.system.store().claim_next(id).unwrap() {
                break input;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    crate::support::relay::wait(|| {
        a.system
            .store()
            .session_summary("core")
            .unwrap()
            .working_actors
            == 1
    })
    .await
    .unwrap();
    let page: Value = a
        .owner(
            reqwest::Method::GET,
            "/v1/sessions/core/actors?status=running&limit=1",
        )
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(page["items"][0]["id"], id);
    assert_eq!(
        page["items"][0]["conversation_url"],
        format!("/stages/{}/actors/{id}", b.stage.host.local)
    );
    let read = actors
        .execute_tool(
            &call(
                "read_conversation",
                json!({"stage_id":b.stage.host.local,"actor_id":id,"limit":10}),
            ),
            &ctx,
        )
        .await
        .unwrap();
    assert!(read.payload.to_string().contains("Inspect the project"));
    assert!(
        actors
            .execute_tool(
                &call(
                    "read_conversation",
                    json!({"stage_id":b.stage.host.local,"actor_id":"root","limit":10})
                ),
                &ctx
            )
            .await
            .is_err()
    );
    let duplicate = actors
        .execute_tool(
            &call(
                "create",
                json!({"stage_id":b.stage.host.local,"name":" remote researcher ","task":null}),
            ),
            &ctx,
        )
        .await;
    assert!(duplicate.is_err());
    b.system
        .store()
        .finish_message(&claimed.id, MessageStatus::Completed, None, None)
        .unwrap();
    crate::support::relay::wait(|| {
        a.system
            .store()
            .session_summary("core")
            .unwrap()
            .working_actors
            == 0
    })
    .await
    .unwrap();
    a.stage.stop().await;
    b.stage.stop().await;
    a.system.shutdown().await;
    b.system.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lifecycle_tombstones_fence_remote_workers_without_affecting_same_named_local_sessions() {
    let relay = crate::support::relay::Relay::new().await;
    let a = Fixture::new().await;
    let b = Fixture::with_config_and_account(a.system.config(), false).await;
    connect(&a, &b, &relay.endpoint).await;
    let ctx = context(&a);
    let mut app = ActorApp::new("core".into(), a.system.clone());
    let created = app
        .execute_tool(
            &call(
                "create",
                json!({"stage_id":b.stage.host.local,"name":"Remote worker","task":"Initial work"}),
            ),
            &ctx,
        )
        .await
        .unwrap();
    let id = created.payload["id"].as_str().unwrap();
    let input = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if let Some(input) = b.system.store().claim_next(id).unwrap() {
                break input;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    a.system.archive_session("core", true).await.unwrap();
    crate::support::relay::wait(|| {
        b.system.store().message(&input.id).unwrap().status == MessageStatus::Cancelled
    })
    .await
    .unwrap();
    assert!(
        b.system
            .send_user("core", "still local".into(), vec![], None)
            .is_ok()
    );
    a.system.archive_session("core", false).await.unwrap();
    a.system.archive_session("core", true).await.unwrap();
    a.system.delete_session("core").await.unwrap();
    let deleted = crate::support::relay::wait(|| b.system.actor(id).is_err()).await;
    deleted.unwrap();
    assert!(b.system.actor("core").is_ok());
    assert!(b.system.actor("root").is_ok());
    a.stage.stop().await;
    b.stage.stop().await;
    a.system.shutdown().await;
    b.system.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn remote_denial_appeal_is_decided_by_session_host_and_notified_on_execution_device() {
    let relay = crate::support::relay::Relay::new().await;
    let a = Fixture::new().await;
    let b = Fixture::with_config_and_account(a.system.config(), false).await;
    connect(&a, &b, &relay.endpoint).await;
    let core_context = context(&a);
    let mut core_app = ActorApp::new("core".into(), a.system.clone());
    let created=core_app.execute_tool(&call("create",json!({"stage_id":b.stage.host.local,"name":"Appeal worker","task":"Perform bounded work"})),&core_context).await.unwrap();
    let worker = created.payload["id"].as_str().unwrap().to_owned();
    let input = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if let Some(input) = b.system.store().claim_next(&worker).unwrap() {
                break input;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let approval = b
        .system
        .store()
        .create_approval(ApprovalRecord {
            seq: 0,
            id: uuid::Uuid::new_v4().to_string(),
            message_id: input.id.clone(),
            call: call(
                "files__write_file",
                json!({"path":"proposal.txt","content":"not replayed"}),
            ),
            requested: daat_locus::sandbox::SandboxScopeRequest {
                readable_roots: vec![],
                writable_roots: vec![b.system.device_workspace().join("granted")],
            },
            sudo: None,
            execution_claim: None,
            scopes: vec![],
            previous_denials: vec![],
            decision: ApprovalDecision::Pending,
            reason: "automatic review denied".into(),
            created_at_ms: chrono::Utc::now().timestamp_millis(),
            resolved_at_ms: None,
            appeal: None,
            application: None,
        })
        .unwrap();
    b.system
        .store()
        .resolve_approval(&approval.id, ApprovalDecision::Denied, "automatic denial")
        .unwrap();
    let worker_context = b
        .system
        .execution_context(&b.system.actor(&worker).unwrap(), &input)
        .unwrap();
    let mut worker_app = ActorApp::new(worker.clone(), b.system.clone());
    worker_app.execute_tool(&call("appeal_approval",json!({"stage_id":b.stage.host.local,"approval_id":approval.id,"explanation":"The user authorized this narrow project output."})),&worker_context).await.unwrap();
    a.system
        .store()
        .finish_message(
            &core_context.actor_call.as_ref().unwrap().message_id,
            MessageStatus::Completed,
            Some("coordinated"),
            None,
        )
        .unwrap();
    let notice = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            if let Some(input) = a.system.store().claim_next("core").unwrap() {
                break input;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let facts: Value = serde_json::from_str(&notice.content).unwrap();
    assert_eq!(facts["type"], "approval_appeal");
    assert_eq!(facts["approval_id"], approval.id);
    let decision_context = a
        .system
        .execution_context(&a.system.actor("core").unwrap(), &notice)
        .unwrap();
    let mut admin = AdminActorsApp::new(a.system.clone());
    let resolved=admin.execute_tool(&call("resolve_approval_appeal",json!({"stage_id":b.stage.host.local,"approval_id":approval.id,"decision":"approve","reason":"The Owner authorized the requested narrow output."})),&decision_context).await.unwrap();
    assert_eq!(resolved.payload["effect_executed"], false);
    let updated = b.system.store().approval(&approval.id).unwrap();
    assert_eq!(
        updated.appeal.as_ref().unwrap().decision,
        ApprovalDecision::Approved
    );
    let notification = b
        .system
        .store()
        .message(
            updated
                .appeal
                .as_ref()
                .unwrap()
                .notification_id
                .as_deref()
                .unwrap(),
        )
        .unwrap();
    assert_eq!(notification.status, MessageStatus::Queued);
    assert!(notification.content.contains("No operation was executed"));
    assert!(!b.system.device_workspace().join("granted").exists());
    assert!(
        a.system.store().approval(&approval.id).is_err(),
        "approval state remains on its execution device"
    );
    a.stage.stop().await;
    b.stage.stop().await;
    a.system.shutdown().await;
    b.system.shutdown().await;
}
