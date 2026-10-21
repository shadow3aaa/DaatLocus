//! Conversation HTTP contracts.
use crate::support::Fixture;
use daat_locus::actors::ActorRecord;
use daat_locus::actors::ActorStatus;
use daat_locus::actors::ApprovalDecision;
use daat_locus::actors::ApprovalMode;
use daat_locus::actors::ApprovalRecord;
use daat_locus::actors::ToolPermissionRequest;
use reqwest::StatusCode;
use serde_json::{Value, json};

#[tokio::test]
async fn actor_catalog_pages_keep_stable_order_without_loading_the_whole_catalog() {
    let fixture = Fixture::new().await;
    for name in ["page-a", "page-b", "page-c", "page-d"] {
        fixture.child(name).await;
    }
    let mut expected: Vec<_> = fixture
        .system
        .list_visible()
        .unwrap()
        .into_iter()
        .map(|actor| actor.id)
        .collect();
    expected.sort();
    let mut received = Vec::new();
    let mut after = None::<String>;
    loop {
        let path = match &after {
            Some(after) => format!("/v1/actors?limit=2&after={after}"),
            None => "/v1/actors?limit=2".into(),
        };
        let response = fixture
            .owner(reqwest::Method::GET, &path)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let page: Value = response.json().await.unwrap();
        let items = page["items"].as_array().unwrap();
        assert!(items.len() <= 2);
        assert!(page["event_cursor"].is_i64());
        for item in items {
            assert!(item.get("config").is_none());
            received.push(item["id"].as_str().unwrap().to_owned());
        }
        after = page["next_cursor"].as_str().map(str::to_owned);
        if after.is_none() {
            break;
        }
        assert!(received.len() < expected.len());
    }
    assert_eq!(received, expected);
}

#[tokio::test]
async fn queue_guidance_is_versioned_scoped_and_survives_a_finishing_target() {
    use daat_locus::actors::{MessageKind, MessageStatus};
    let fixture = Fixture::new().await;
    let actor = fixture.child("guidance").await;
    let target = fixture
        .system
        .send_user(&actor, "current task".into(), vec![], None)
        .unwrap();
    fixture.system.store().claim_next(&actor).unwrap().unwrap();
    let input = fixture
        .system
        .send_user(&actor, "guide this task".into(), vec![], None)
        .unwrap();
    let path = format!("/v1/actors/{actor}/messages/{}/steer", input.id);
    assert_eq!(
        fixture
            .client
            .post(format!("{}{path}", fixture.url))
            .header("origin", &fixture.url)
            .header("x-daat-request", "1")
            .json(
                &json!({"target_message_id":target.id,"expected_updated_at_ms":input.updated_at_ms})
            )
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(fixture.owner(reqwest::Method::POST,&path).json(&json!({"target_message_id":target.id,"expected_updated_at_ms":input.updated_at_ms-1})).send().await.unwrap().status(),StatusCode::CONFLICT);
    let queued = fixture
        .owner(
            reqwest::Method::GET,
            &format!("/v1/actors/{actor}/queue?limit=1"),
        )
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(queued["items"][0]["id"], input.id);
    assert_eq!(
        fixture
            .owner(reqwest::Method::POST, &path)
            .json(
                &json!({"target_message_id":target.id,"expected_updated_at_ms":input.updated_at_ms})
            )
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        fixture.system.store().message(&input.id).unwrap().kind,
        MessageKind::Steering
    );
    fixture
        .system
        .store()
        .finish_message(&target.id, MessageStatus::Completed, Some("finished"), None)
        .unwrap();
    let restored = fixture.system.store().message(&input.id).unwrap();
    assert_eq!(restored.kind, MessageKind::Request);
    assert_eq!(restored.status, MessageStatus::Queued);
    assert_eq!(restored.content, "guide this task");
    assert_eq!(
        fixture
            .system
            .store()
            .claim_next(&actor)
            .unwrap()
            .unwrap()
            .id,
        input.id
    );
    fixture.system.shutdown().await;
}

#[tokio::test]
async fn queued_edit_withdraw_and_clear_are_atomic_and_do_not_steer_running_work() {
    let fixture = Fixture::new().await;
    let send = |content: &str| {
        fixture
            .system
            .send_user("root", content.into(), vec![], None)
            .unwrap()
    };
    let active = send("active");
    fixture.system.store().claim_next("root").unwrap().unwrap();
    let first = send("queued one");
    let second = send("queued two");
    let edited = fixture.owner(reqwest::Method::POST, &format!("/v1/actors/root/messages/{}/edit",first.id)).json(&json!({"content":"updated request","attachments":[],"expected_updated_at_ms":first.updated_at_ms})).send().await.unwrap();
    assert_eq!(edited.status(), StatusCode::OK);
    let edited = edited.json::<Value>().await.unwrap();
    assert_eq!(edited["content"], "updated request");
    assert_eq!(edited["source"]["kind"], "user");
    assert_eq!(fixture.owner(reqwest::Method::POST, &format!("/v1/actors/root/messages/{}/edit",first.id)).json(&json!({"content":"stale overwrite","attachments":[],"expected_updated_at_ms":first.updated_at_ms})).send().await.unwrap().status(), StatusCode::CONFLICT);
    assert_eq!(fixture.owner(reqwest::Method::POST, &format!("/v1/actors/root/messages/{}/withdraw",active.id)).json(&json!({"expected_updated_at_ms":fixture.system.store().message(&active.id).unwrap().updated_at_ms})).send().await.unwrap().status(),StatusCode::CONFLICT);
    let cleared = fixture
        .owner(reqwest::Method::POST, "/v1/actors/root/queue/clear")
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(cleared["cancelled_count"], 2);
    let running = fixture.system.store().message(&active.id).unwrap();
    assert_eq!(running.status, daat_locus::actors::MessageStatus::Running);
    assert!(!running.cancel_requested);
    assert_eq!(
        fixture.system.store().message(&second.id).unwrap().status,
        daat_locus::actors::MessageStatus::Cancelled
    );
    assert_eq!(
        fixture
            .owner(
                reqwest::Method::GET,
                &format!("/v1/actors/root/messages/{}/activity?limit=1", first.id)
            )
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn queued_clear_preserves_actor_requests_and_started_work() {
    let fixture = Fixture::new().await;
    let actor = fixture.child("queue-target").await;
    let root = fixture
        .system
        .send_user("root", "delegate".into(), vec![], None)
        .unwrap();
    fixture.system.store().claim_next("root").unwrap().unwrap();
    let delegated = fixture
        .system
        .send_from_actor("root", &root.id, &actor, "collaboration".into())
        .unwrap();
    let own = fixture
        .system
        .send_user(&actor, "user input".into(), vec![], None)
        .unwrap();
    assert_eq!(
        fixture.system.store().clear_queued_users(&actor).unwrap(),
        1
    );
    assert_eq!(
        fixture
            .system
            .store()
            .message(&delegated.id)
            .unwrap()
            .status,
        daat_locus::actors::MessageStatus::Queued
    );
    assert_eq!(
        fixture.system.store().message(&own.id).unwrap().status,
        daat_locus::actors::MessageStatus::Cancelled
    );
    let claimed = fixture.system.store().claim_next(&actor).unwrap().unwrap();
    assert!(
        fixture
            .system
            .store()
            .amend_queued_user(
                &actor,
                &claimed.id,
                claimed.updated_at_ms,
                "spoof actor".into(),
                vec![],
                false
            )
            .is_err()
    );
}

#[tokio::test]
async fn chat_search_matches_original_content_and_reply_with_exact_actor_scope() {
    let fixture = Fixture::new().await;
    let a = fixture.child("search-project").await;
    let b = fixture.child("other-project").await;
    for (content, reply) in [
        ("Needle in content", "first reply"),
        ("ordinary request", "NEEDLE in reply"),
        ("literal % and _ plus 中文", "fixture-secret-key"),
        ("another needle", "another reply"),
    ] {
        let message = fixture
            .system
            .send_user(&a, content.into(), vec![], None)
            .unwrap();
        fixture
            .system
            .store()
            .finish_message(
                &message.id,
                daat_locus::actors::MessageStatus::Completed,
                Some(reply),
                None,
            )
            .unwrap();
    }
    fixture
        .system
        .send_user(&b, "needle in another Actor".into(), vec![], None)
        .unwrap();
    // Newer unmatched rows must not starve older matching rows before LIMIT.
    for _ in 0..5 {
        fixture
            .system
            .send_user(&a, "unmatched".into(), vec![], None)
            .unwrap();
    }
    let path = format!("/v1/actors/{a}/messages/search?q=needle&limit=2");
    let page = fixture
        .owner(reqwest::Method::GET, &path)
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(page["items"].as_array().unwrap().len(), 2);
    assert!(page["event_cursor"].is_i64());
    let before = page["next_cursor"].as_i64().unwrap();
    let older = fixture
        .owner(reqwest::Method::GET, &format!("{path}&before={before}"))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(older["items"].as_array().unwrap().len(), 1);
    assert!(older["next_cursor"].is_null());
    let literal = fixture
        .owner(
            reqwest::Method::GET,
            &format!("/v1/actors/{a}/messages/search?q=%25"),
        )
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(literal["items"].as_array().unwrap().len(), 1);
    assert_eq!(literal["items"][0]["reply"], "[redacted]");
    for query in ["q=", "q=needle&limit=101", "q=needle&before=0"] {
        assert_eq!(
            fixture
                .owner(
                    reqwest::Method::GET,
                    &format!("/v1/actors/{a}/messages/search?{query}")
                )
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
}

#[tokio::test]
async fn queries_are_bounded_and_every_actor_has_a_link_without_catalog_configuration() {
    let fixture = Fixture::new().await;
    let child = fixture.child("alpha").await;
    let parent = fixture.system.actor(&child).unwrap();
    let mut config = parent.config.clone();
    config.instructions = "private instructions ".repeat(10_000);
    let spawned = ActorRecord {
        remote_read_only: false,
        context: Default::default(),
        id: "spawned".into(),
        parent_id: Some(child),
        depth: 2,
        config,
        status: ActorStatus::Idle,
        created_at_ms: 1,
        updated_at_ms: 1,
    };
    fixture.system.store().save_actor(&spawned).unwrap();
    let detail = fixture
        .owner(reqwest::Method::GET, "/v1/actors/spawned")
        .send()
        .await
        .unwrap();
    assert_eq!(detail.status(), StatusCode::OK);
    let detail = detail.json::<Value>().await.unwrap();
    assert_eq!(detail["conversation_url"], "/actors/spawned");
    assert!(detail.get("depth").is_none());
    let directory = fixture
        .owner(reqwest::Method::GET, "/v1/actors?limit=100")
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert!(
        directory["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["id"] == "spawned" && a["conversation_url"] == "/actors/spawned")
    );
    let page = fixture
        .owner(reqwest::Method::GET, "/v1/actors?limit=1")
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(page["items"].as_array().unwrap().len(), 1);
    assert!(page["next_cursor"].is_string());
    for path in [
        "/v1/actors?limit=0",
        "/v1/events?limit=101",
        "/v1/actors/root/messages?limit=101",
        "/v1/events?after=-1",
    ] {
        assert_eq!(
            fixture
                .owner(reqwest::Method::GET, path)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    let events = fixture
        .owner(reqwest::Method::GET, "/v1/events?limit=100")
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    let text = events.to_string();
    assert!(!text.contains("private instructions") && !text.contains("readable_roots"));
    assert!(text.contains("/actors/spawned"));
    let after = events["next_cursor"].as_i64().unwrap();
    let empty = fixture
        .owner(reqwest::Method::GET, &format!("/v1/events?after={after}"))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert!(empty["items"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn actor_creation_constraints_cannot_be_rewritten_through_client_management() {
    let fixture = Fixture::new().await;
    let actor = fixture.child("fixed-purpose").await;
    let original = fixture.system.actor(&actor).unwrap().config;
    let endpoint = format!("/v1/actors/{actor}");
    assert_eq!(
        fixture
            .owner(reqwest::Method::POST, "/v1/actors")
            .json(&original)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::METHOD_NOT_ALLOWED
    );
    let mut changed = original.clone();
    changed.instructions = "Replace identity".into();
    assert_eq!(
        fixture
            .owner(reqwest::Method::PUT, &endpoint)
            .json(&changed)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::METHOD_NOT_ALLOWED
    );
    for change in [
        json!({"permissions":{"readable_roots":["C:/"]}}),
        json!({"workspace":"C:/"}),
        json!({"apps":["terminal"]}),
        json!({"instructions":"replace identity"}),
        json!({"name":"changed"}),
        json!({"approval_mode":"none"}),
        json!({"approval_mode":"none","permissions":{"allow_children":true}}),
    ] {
        assert_eq!(
            fixture
                .owner(reqwest::Method::PATCH, &endpoint)
                .json(&change)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    assert_eq!(fixture.system.actor(&actor).unwrap().config, original);
    for mode in ["none", "ask", "auto"] {
        let response = fixture
            .owner(reqwest::Method::PATCH, "/v1/actors/core")
            .json(&json!({"approval_mode":mode}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let actual = fixture.system.actor(&actor).unwrap().config;
        let mut expected = original.clone();
        expected.permissions.approval_mode = serde_json::from_value(json!(mode)).unwrap();
        assert_eq!(actual, expected);
    }
}

#[tokio::test]
async fn owner_can_open_and_control_created_actors() {
    let fixture = Fixture::new().await;
    let a = fixture.child("project").await;
    let mut config = fixture.system.actor(&a).unwrap().config;
    config.permissions.approval_mode = ApprovalMode::Ask;
    fixture.system.update_owned(&a, config).await.unwrap();
    let mut internal = fixture.system.actor(&a).unwrap();
    internal.id = "internal-worker".into();
    internal.parent_id = Some(a.clone());
    internal.depth = 2;
    fixture.system.store().save_actor(&internal).unwrap();
    let mut created = internal.clone();
    created.id = "created-by-worker".into();
    created.parent_id = Some(internal.id.clone());
    created.depth = 3;
    fixture.system.store().save_actor(&created).unwrap();
    // Seed an owned Actor's delegated input directly; no model is invoked.
    let mut input = fixture
        .system
        .send_user(&a, "permission test".into(), vec![], None)
        .unwrap();
    let parent_request = input.id.clone();
    input.seq = 0;
    input.id = uuid::Uuid::new_v4().to_string();
    input.actor_id = internal.id.clone();
    input.source = daat_locus::actors::MessageSource::Actor {
        actor_id: a.clone(),
    };
    input.reply_to = daat_locus::actors::ReplyTarget::Actor {
        actor_id: a.clone(),
        request_id: parent_request,
    };
    fixture.system.store().enqueue(input, None).unwrap();
    let input = fixture
        .system
        .store()
        .claim_next(&internal.id)
        .unwrap()
        .unwrap();
    let approval = fixture
        .system
        .store()
        .create_approval(ApprovalRecord {
            seq: 0,
            id: uuid::Uuid::new_v4().to_string(),
            actor_id: internal.id.clone(),
            message_id: input.id,
            call: daat_locus::model_protocol::AgentToolCall {
                id: "internal-action".into(),
                name: "files__read_file".into(),
                arguments: json!({"path": "README.md"}),
            },
            requested: ToolPermissionRequest::default(),
            review: None,
            scopes: vec![],
            mode: ApprovalMode::Ask,
            decision: ApprovalDecision::Pending,
            reason: "Owner approval required".into(),
            created_at_ms: 1,
            resolved_at_ms: None,
        })
        .unwrap();
    assert_eq!(
        fixture
            .owner(
                reqwest::Method::GET,
                &format!("/v1/actors/{}/messages", internal.id)
            )
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        fixture
            .owner(
                reqwest::Method::GET,
                &format!("/v1/actors/{}/approvals/{}", internal.id, approval.id)
            )
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let global = fixture
        .owner(reqwest::Method::GET, "/v1/approvals?awaiting_owner=true")
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(global["items"][0]["id"], approval.id);
    let events = fixture
        .owner(reqwest::Method::GET, "/v1/events?limit=100")
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert!(
        events["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["event"]["approval_id"] == approval.id)
    );
    assert_eq!(
        fixture
            .owner(
                reqwest::Method::GET,
                &format!("/v1/approvals/{}", approval.id)
            )
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        fixture
            .owner(
                reqwest::Method::POST,
                &format!("/v1/approvals/{}", approval.id)
            )
            .json(&json!({"decision": "deny"}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let details = fixture
        .owner(reqwest::Method::GET, &format!("/v1/actors/{}", internal.id))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(
        details["conversation_url"],
        format!("/actors/{}", internal.id)
    );
}
