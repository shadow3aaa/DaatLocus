// Included in approval::tests to reuse its isolated reviewer and Actor fixtures.
use crate::actors::runtime_permissions::{self as runtime_permission, ApplicationSource};

fn runtime_application() -> super::super::ReviewApplication {
    super::super::ReviewApplication {
        task: "Inspect the requested fixture".into(),
        reason: "Read the exact fixture directory reported by the tool".into(),
    }
}
fn runtime_call() -> AgentToolCall {
    AgentToolCall {
        id: "runtime-fixture".into(),
        name: "fixture__inspect".into(),
        arguments: json!({"opaque":"unchanged original argument"}),
    }
}
fn completed_reference(
    system: &ActorSystem,
    actor: &ActorRecord,
    message: &MessageRecord,
    call: &AgentToolCall,
) -> String {
    let id = system
        .store()
        .tool_started(&actor.id, &message.id, call)
        .unwrap();
    system
        .store()
        .tool_finished_with_reference(
            &actor.id,
            &message.id,
            call,
            json!({"output":"fixture Permission denied","exit_code":0,"running":false}),
            "fixture Permission denied",
            "Fixture result",
            None,
            Some(&id),
        )
        .unwrap();
    id
}
fn result_source(id: &str, root: &std::path::Path) -> ApplicationSource {
    ApplicationSource::ToolResult {
        tool_result_id: id.into(),
        readable_roots: vec![root.into()],
        writable_roots: vec![],
    }
}

#[tokio::test]
async fn runtime_result_reference_uses_auto_ask_and_full_access_without_replaying() {
    for mode in [ApprovalMode::Auto, ApprovalMode::Ask, ApprovalMode::None] {
        let reviewer = Arc::new(Reviewer::new(ReviewReply::Json(
            json!({"decision":"approve","reason":"Exact requested fixture"}),
        )));
        let (dir, system, actor, message) = fixture(mode, reviewer.clone()).await;
        let root = dir.path().join("additional");
        std::fs::create_dir(&root).unwrap();
        let base = system.execution_context(&actor, &message).unwrap();
        let call = runtime_call();
        let id = completed_reference(&system, &actor, &message, &call);
        assert!(!base.sandbox_policy.is_path_readable(&root));
        let control = ActorControl::default();
        let submit = runtime_permission::submit(
            &system,
            &actor,
            &message,
            &control,
            &base,
            result_source(&id, &root),
            runtime_application(),
        );
        let owner = async {
            if mode == ApprovalMode::Ask {
                tokio::time::timeout(Duration::from_secs(3), async {
                    loop {
                        let pending = system
                            .store()
                            .pending_approvals(Some(&actor.id), None, 10)
                            .unwrap();
                        if let Some(record) = pending.items.first() {
                            system
                                .store()
                                .resolve_approval(
                                    &record.id,
                                    ApprovalDecision::Approved,
                                    "Exact requested fixture",
                                )
                                .unwrap();
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .unwrap();
            }
        };
        let (record, ()) = tokio::time::timeout(Duration::from_secs(4), async {
            tokio::join!(submit, owner)
        })
        .await
        .unwrap();
        let record = record.unwrap();
        assert_eq!(record.decision, ApprovalDecision::Approved);
        assert_eq!(record.call.arguments, call.arguments);
        assert_eq!(record.call.name, call.name);
        assert_eq!(
            record.requested.readable_roots,
            vec![root.canonicalize().unwrap()]
        );
        assert!(record.requested.writable_roots.is_empty());
        assert_eq!(
            reviewer.requests.lock().len(),
            usize::from(mode == ApprovalMode::Auto)
        );
        let next = system.execution_context(&actor, &message).unwrap();
        assert!(next.sandbox_policy.is_path_readable(&root));
        assert!(!next.sandbox_policy.is_path_writable(&root));
        assert!(
            !next
                .sandbox_policy
                .is_path_readable(&dir.path().join("other"))
        );
        let saved = system.store().tool_result(&id).unwrap();
        assert_eq!(
            saved.payload,
            json!({"output":"fixture Permission denied","exit_code":0,"running":false})
        );
        assert_eq!(
            system
                .store()
                .tool_summaries(&actor.id, &message.id, None, 10)
                .unwrap()
                .items
                .len(),
            1,
            "requesting permission creates no execution"
        );
        system.shutdown().await;
    }
}

#[tokio::test]
async fn runtime_result_denied_or_unavailable_review_never_remembers_scope() {
    for reply in [
        ReviewReply::Json(json!({"decision":"deny","reason":"Not needed"})),
        ReviewReply::Fail,
        ReviewReply::Timeout,
    ] {
        let reviewer = Arc::new(Reviewer::new(reply));
        let (dir, system, actor, message) =
            fixture(ApprovalMode::Auto, reviewer.clone()).await;
        let root = dir.path().join("additional");
        std::fs::create_dir(&root).unwrap();
        let base = system.execution_context(&actor, &message).unwrap();
        let id = completed_reference(&system, &actor, &message, &runtime_call());
        assert!(
            runtime_permission::submit(
                &system,
                &actor,
                &message,
                &ActorControl::default(),
                &base,
                result_source(&id, &root),
                runtime_application()
            )
            .await
            .is_err()
        );
        assert!(
            !system
                .execution_context(&actor, &message)
                .unwrap()
                .sandbox_policy
                .is_path_readable(&root)
        );
        assert_eq!(
            system
                .store()
                .approvals(Some(&actor.id), None, 10)
                .unwrap()
                .items[0]
                .decision,
            ApprovalDecision::Denied
        );
        let requested = ToolPermissionRequest { readable_roots: vec![root.clone()], ..Default::default() };
        let review = AppReview::new("fixture/new-effect/v1", "Review this operation", json!({"target":"fixture"}));
        let retry = super::super::approval::authorize(&system, &actor, &message, &runtime_call(), &ActorControl::default(), &base, &requested, Some(&review)).await.err().expect("refused scope must remain denied");
        assert!(rejection(&retry).is_some());
        assert_eq!(reviewer.requests.lock().len(), 1, "an App review cannot bypass a refused filesystem scope");
        let appeal = runtime_permission::submit(
            &system, &actor, &message, &ActorControl::default(), &base,
            result_source(&id, &root), runtime_application(),
        ).await.unwrap();
        assert!(appeal.appeal.is_some());
        assert_eq!(reviewer.requests.lock().len(), 1, "a denied result routes to Core without another review");
        system.shutdown().await;
    }
}

#[tokio::test]
async fn reviewed_denial_is_reused_and_one_control_routes_it_to_core() {
    use crate::app::App;
    for mode in [ApprovalMode::Auto, ApprovalMode::Ask] {
        let reviewer = Arc::new(Reviewer::new(ReviewReply::Json(json!({"decision":"deny","reason":"Fixture scope refused"}))));
        let (_dir, system, actor, message) = fixture(mode, reviewer.clone()).await;
        let base = system.execution_context(&actor, &message).unwrap();
        let mut call = runtime_call();
        let review = AppReview::new("fixture/once/v1", "Review the fixture effect", json!({"target":"once"}));
        let control = ActorControl::default();
        let requested = ToolPermissionRequest::default();
        let authorize = super::super::approval::authorize(&system, &actor, &message, &call, &control, &base, &requested, Some(&review));
        let owner = async {
            if mode == ApprovalMode::Ask {
                let record = approval(&system, &message.id).await;
                system.store().resolve_approval(&record.id, ApprovalDecision::Denied, "Fixture scope refused").unwrap();
            }
        };
        let (result, ()) = tokio::join!(authorize, owner);
        let first = rejection(&result.err().expect("review must deny")).unwrap();
        call.id = "different-call-id".into();
        let second = tokio::time::timeout(Duration::from_secs(3), super::super::approval::authorize(&system, &actor, &message, &call, &control, &base, &requested, Some(&review)))
            .await.expect("a denied retry must not start another review").err().expect("retry must preserve denial");
        assert_eq!(rejection(&second).unwrap().approval_id, first.approval_id);
        assert_eq!(system.store().approvals(Some(&actor.id), None, 10).unwrap().items.len(), 1);
        let reviews = reviewer.requests.lock().len();
        assert_eq!(reviews, usize::from(mode == ApprovalMode::Auto));
        let mut actors = crate::actor_app::ActorApp::new(actor.id.clone(), system.clone());
        let appeal_call = AgentToolCall {
            id: "appeal-fixture".into(), name: "appeal_approval".into(),
            arguments: json!({"stage_id":null,"approval_id":first.approval_id,"tool_result_id":null,"readable_roots":null,"writable_roots":null,"explanation":"Ask the Owner about the fixture permission"}),
        };
        let sent = actors.execute_tool(&appeal_call, &base).await.unwrap();
        let duplicate = actors.execute_tool(&appeal_call, &base).await.unwrap();
        assert_eq!(sent.payload["core_message_id"], duplicate.payload["core_message_id"]);
        assert_eq!(sent.payload["blocking"], false);
        assert_eq!(reviewer.requests.lock().len(), reviews);
        let record = system.store().approval(&first.approval_id).unwrap();
        assert_eq!(record.decision, ApprovalDecision::Denied);
        assert!(record.appeal.is_some());
        system.shutdown().await;
    }
}

#[tokio::test]
async fn runtime_result_reference_rejects_imports_changed_bindings_and_inactive_authority() {
    let (_, system, actor, message) = fixture(
        ApprovalMode::None,
        Arc::new(Reviewer::new(ReviewReply::Fail)),
    )
    .await;
    let call = runtime_call();
    let id = system
        .store()
        .tool_started(&actor.id, &message.id, &call)
        .unwrap();
    assert!(
        system
            .store()
            .native_tool_result(&id, &actor.id, &message.id)
            .is_err(),
        "started but unfinished is not consumable"
    );
    system
        .store()
        .tool_finished(
            &actor.id,
            &message.id,
            &call,
            json!({"output":"original"}),
            "original",
            "Fixture",
            None,
        )
        .unwrap();
    assert!(
        system
            .store()
            .native_tool_result(&id, &actor.id, &message.id)
            .is_err(),
        "an imported or recovered done observation cannot confirm a native result"
    );
    system
        .store()
        .tool_finished_with_reference(
            &actor.id,
            &message.id,
            &call,
            json!({"output":"original"}),
            "original",
            "Fixture",
            None,
            Some(&id),
        )
        .unwrap();
    assert!(
        system
            .store()
            .native_tool_result(&id, &actor.id, &message.id)
            .is_ok()
    );
    assert!(
        system
            .store()
            .native_tool_result(&id, "another-actor", &message.id)
            .is_err()
    );
    assert!(
        system
            .store()
            .native_tool_result(&id, &actor.id, "another-message")
            .is_err()
    );
    let original = system.store().tool_result(&id).unwrap();
    for field in [
        "arguments",
        "name",
        "actor_id",
        "message_id",
        "call_id",
        "started_at_ms",
    ] {
        let mut value = serde_json::to_value(&original).unwrap();
        value[field] = if field == "arguments" {
            json!({"opaque":"changed imported call"})
        } else if field == "started_at_ms" {
            json!(0)
        } else {
            json!("changed")
        };
        system
            .store()
            .stage_transaction(|tx| {
                tx.execute(
                    "UPDATE tools SET record=?1 WHERE id=?2",
                    rusqlite::params![value.to_string(), id],
                )?;
                Ok(())
            })
            .unwrap();
        assert!(
            system
                .store()
                .native_tool_result(&id, &actor.id, &message.id)
                .is_err(),
            "copied same UUID with changed {field}"
        );
    }
    system
        .store()
        .stage_transaction(|tx| {
            tx.execute(
                "UPDATE tools SET record=?1 WHERE id=?2",
                rusqlite::params![serde_json::to_string(&original).unwrap(), id],
            )?;
            Ok(())
        })
        .unwrap();
    let imported = AgentToolCall {
        id: "imported-without-start".into(),
        ..call
    };
    let imported_id = system
        .store()
        .tool_finished(
            &actor.id,
            &message.id,
            &imported,
            json!({}),
            "copied",
            "Copy",
            None,
        )
        .unwrap();
    assert!(
        system
            .store()
            .native_tool_result(&imported_id, &actor.id, &message.id)
            .is_err()
    );
    system.store().stage_transaction(|tx|{tx.execute("UPDATE messages SET record=json_set(record,'$.remote_authority',json(?1)) WHERE id=?2",rusqlite::params![json!({"peer":crate::stage::StageId(Uuid::new_v4()),"grant_actor":"remote","reply_for":null}).to_string(),message.id])?;Ok(())}).unwrap();
    assert!(
        system
            .store()
            .native_tool_result(&id, &actor.id, &message.id)
            .is_err(),
        "private message authority changed"
    );
    system.shutdown().await;
}

#[tokio::test]
async fn runtime_result_reference_does_not_claim_covered_scope_or_plain_errno_is_a_denial() {
    let (_, system, actor, message) = fixture(
        ApprovalMode::None,
        Arc::new(Reviewer::new(ReviewReply::Fail)),
    )
    .await;
    let call = runtime_call();
    let base = system.execution_context(&actor, &message).unwrap();
    let id = completed_reference(&system, &actor, &message, &call);
    let covered = runtime_permission::submit(
        &system,
        &actor,
        &message,
        &ActorControl::default(),
        &base,
        result_source(&id, &actor.config.workspace),
        runtime_application(),
    )
    .await
    .unwrap_err();
    assert!(covered.to_string().contains("already covered"));
    assert!(
        system
            .store()
            .approvals(Some(&actor.id), None, 10)
            .unwrap()
            .items
            .is_empty()
    );
    let error = miette!("Permission denied (os error 5) for unknown native target");
    let mut result = runtime_permission::error_result(&system, &error);
    let payload = result.payload.clone();
    let content = result.model_content.clone();
    runtime_permission::attach_reference(
        &system,
        &actor,
        &message,
        &call,
        &base,
        &id,
        &mut result,
        Some(&error),
    )
    .unwrap();
    assert_eq!(result.payload, payload);
    assert_eq!(result.model_content, content);
    assert!(result.permission_reference.unwrap().approval_id.is_none());
    assert!(
        system
            .store()
            .approvals(Some(&actor.id), None, 10)
            .unwrap()
            .items
            .is_empty()
    );
    system.shutdown().await;
}

#[tokio::test]
async fn runtime_typed_denial_creates_exact_immutable_proposal_and_respects_readonly() {
    let (dir, system, actor, message) = fixture(
        ApprovalMode::None,
        Arc::new(Reviewer::new(ReviewReply::Fail)),
    )
    .await;
    let root = dir.path().join("additional");
    std::fs::create_dir(&root).unwrap();
    let call = runtime_call();
    let base = system.execution_context(&actor, &message).unwrap();
    let id = completed_reference(&system, &actor, &message, &call);
    let error = base
        .sandbox_policy
        .ensure_path_readable(&root, "fixture input")
        .unwrap_err()
        .wrap_err("fixture operation failed");
    let mut result = runtime_permission::error_result(&system, &error);
    runtime_permission::attach_reference(
        &system,
        &actor,
        &message,
        &call,
        &base,
        &id,
        &mut result,
        Some(&error),
    )
    .unwrap();
    let approval_id = result.permission_reference.unwrap().approval_id.unwrap();
    let proposal = system.store().approval(&approval_id).unwrap();
    assert_eq!(proposal.decision, ApprovalDecision::ApplicationRequired);
    assert_eq!(
        proposal.requested.readable_roots,
        vec![root.canonicalize().unwrap()]
    );
    assert_eq!(proposal.call.arguments, call.arguments);
    assert!(
        !system
            .execution_context(&actor, &message)
            .unwrap()
            .sandbox_policy
            .is_path_readable(&root)
    );
    let mut readonly = base.clone();
    readonly.actor_call.as_mut().unwrap().remote_read_only = true;
    let error = readonly
        .sandbox_policy
        .ensure_path_writable(&root, "fixture output")
        .unwrap_err();
    let mut blocked = runtime_permission::error_result(&system, &error);
    runtime_permission::attach_reference(
        &system,
        &actor,
        &message,
        &call,
        &readonly,
        &id,
        &mut blocked,
        Some(&error),
    )
    .unwrap();
    assert!(
        blocked.permission_reference.unwrap().approval_id.is_none(),
        "read-only execution cannot publish a write approval ID"
    );
    let result = runtime_permission::submit(
        &system,
        &actor,
        &message,
        &ActorControl::default(),
        &base,
        ApplicationSource::Approval { approval_id },
        runtime_application(),
    )
    .await
    .unwrap();
    assert_eq!(result.decision, ApprovalDecision::Approved);
    assert!(
        system
            .execution_context(&actor, &message)
            .unwrap()
            .sandbox_policy
            .is_path_readable(&root)
    );
    system.shutdown().await;
}

#[tokio::test]
async fn runtime_result_reference_is_removed_by_account_reset() {
    let (_, system, actor, message) = fixture(
        ApprovalMode::None,
        Arc::new(Reviewer::new(ReviewReply::Fail)),
    )
    .await;
    let id = completed_reference(&system, &actor, &message, &runtime_call());
    let host = crate::stage::host::Host::load(system.clone()).unwrap();
    host.initialize(true).unwrap();
    let runtime = crate::stage::runtime::Runtime::new(host);
    runtime.reset_account_work(None).await.unwrap();
    assert!(
        system
            .store()
            .native_tool_result(&id, &actor.id, &message.id)
            .is_err()
    );
    let count: i64 = system
        .store()
        .stage_transaction(|tx| {
            Ok(
                tx.query_row("SELECT count(*) FROM stage_local_tool_results", [], |r| {
                    r.get(0)
                })?,
            )
        })
        .unwrap();
    assert_eq!(count, 0);
    system.shutdown().await;
}

#[tokio::test]
async fn runtime_shared_result_reference_stays_on_source_device_and_rejects_unknown_effect() {
    let (dir, system, actor, message) = fixture(
        ApprovalMode::None,
        Arc::new(Reviewer::new(ReviewReply::Fail)),
    )
    .await;
    let (_other_dir, other, other_actor, other_message) = fixture(
        ApprovalMode::None,
        Arc::new(Reviewer::new(ReviewReply::Fail)),
    )
    .await;
    let root = dir.path().join("additional");
    std::fs::create_dir(&root).unwrap();
    let base = system.execution_context(&actor, &message).unwrap();
    let call = runtime_call();
    system.store().stage_transaction(|tx|{tx.execute_batch("CREATE TABLE core_app_effects(message TEXT NOT NULL,call TEXT NOT NULL,digest TEXT NOT NULL,state TEXT NOT NULL,result BLOB,PRIMARY KEY(message,call));")?;Ok(())}).unwrap();
    let id = system
        .store()
        .shared_tool_started(&actor.id, &message.id, &call, false, "source-effect-digest")
        .unwrap();
    assert!(
        system
            .store()
            .native_tool_result(&id, &actor.id, &message.id)
            .is_err()
    );
    let mut result = crate::app::AppToolExecutionResult::new(
        "Fixture",
        json!({"output":"native stderr unchanged","running":false,"exit_code":5}),
        None,
    );
    runtime_permission::attach_reference(
        &system,
        &actor,
        &message,
        &call,
        &base,
        &id,
        &mut result,
        None,
    )
    .unwrap();
    let bytes = serde_json::to_vec(&result).unwrap();
    for (state, digest, valid) in [
        ("unknown", "source-effect-digest", false),
        ("started", "source-effect-digest", false),
        ("completed", "different-effect", false),
        ("completed", "source-effect-digest", true),
    ] {
        system.store().stage_transaction(|tx|{tx.execute("UPDATE core_app_effects SET state=?1,digest=?2,result=?3 WHERE message=?4 AND call=?5",rusqlite::params![state,digest,bytes,message.id,call.id])?;Ok(())}).unwrap();
        assert_eq!(
            system
                .store()
                .native_tool_result(&id, &actor.id, &message.id)
                .is_ok(),
            valid
        );
    }
    // A replicated result is readable, but never imports the execution provenance.
    let copied = other
        .store()
        .tool_finished(
            &other_actor.id,
            &other_message.id,
            &call,
            result.payload.clone(),
            "copied source result",
            "Fixture",
            None,
        )
        .unwrap();
    assert!(
        other
            .store()
            .native_tool_result(&copied, &other_actor.id, &other_message.id)
            .is_err()
    );
    assert!(
        runtime_permission::submit(
            &other,
            &other_actor,
            &other_message,
            &ActorControl::default(),
            &other
                .execution_context(&other_actor, &other_message)
                .unwrap(),
            result_source(&id, &root),
            runtime_application()
        )
        .await
        .is_err()
    );
    runtime_permission::submit(
        &system,
        &actor,
        &message,
        &ActorControl::default(),
        &base,
        result_source(&id, &root),
        runtime_application(),
    )
    .await
    .unwrap();
    assert!(
        system
            .execution_context(&actor, &message)
            .unwrap()
            .sandbox_policy
            .is_path_readable(&root)
    );
    assert!(
        other
            .store()
            .filesystem_approval_scopes(&other_actor.id)
            .unwrap()
            .is_empty()
    );
    system.shutdown().await;
    other.shutdown().await;
}
