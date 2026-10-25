use super::*;

async fn wait_requests(provider: &ReplayProvider, count: usize) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while provider.requests.lock().len() < count {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("incoming message did not reach inference");
}

#[tokio::test]
async fn core_explicitly_selects_task_link_visibility_without_changing_message_delivery() {
    let (_directory, system, _) = paused_runtime().await;
    system.install_local_core().unwrap();
    system
        .send_user("core", "Coordinate work".into(), vec![], None)
        .unwrap();
    let input = system.store().claim_next("core").unwrap().unwrap();
    let actor = system.execution_actor(&input).unwrap();
    let context = system.execution_context(&actor, &input).unwrap();
    let mut apps =
        crate::builtin_apps::create_apps(&actor, system.clone(), &system.config()).unwrap();
    let mut call = AgentToolCall {
        id: "missing-choice".into(),
        name: "actors__message".into(),
        arguments: json!({"stage_id":null,"actor_id":"root","reply_to":null,"content":"Coordinate"}),
    };
    assert!(apps.validate_tool_call(&call, &context).is_err());
    assert!(
        system
            .store()
            .messages("root", None, 10)
            .unwrap()
            .items
            .is_empty()
    );
    for (id, show) in [("initial-delegation", true), ("internal-followup", false)] {
        call.id = id.into();
        call.arguments["show_to_user"] = json!(show);
        call.arguments["content"] = json!(id);
        let result = apps.execute_tool(&call, &context).await.unwrap();
        system
            .store()
            .tool_finished(
                &actor.id,
                &input.id,
                &call,
                result.payload,
                result.model_content.as_deref().unwrap_or(""),
                &result.summary,
                None,
            )
            .unwrap();
    }
    let summaries = system
        .store()
        .tool_summaries("core", &input.id, None, 10)
        .unwrap()
        .items;
    assert_eq!(summaries.len(), 2);
    for (summary, expected) in summaries.iter().zip([true, false]) {
        assert!(summary.error.is_none(), "{:?}", summary.error);
        let outcome = summary.outcome.as_ref().unwrap();
        assert_eq!(outcome["show_to_user"], json!(expected));
        let delivered = system
            .store()
            .message(outcome["message_id"].as_str().unwrap())
            .unwrap();
        assert_eq!(delivered.actor_id, "root");
        assert_eq!(delivered.content, summary.call_id);
        assert_eq!(delivered.status, MessageStatus::Queued);
        assert!(
            matches!(delivered.reply_to, crate::actors::ReplyTarget::Actor { actor_id, request_id } if actor_id == "core" && request_id == input.id)
        );
    }
    let events = system.store().events(0, Some("core"), false, 100).unwrap();
    let choices: Vec<_> = events
        .iter()
        .filter_map(|record| match &record.event {
            ActorEvent::ToolFinished {
                outcome: Some(outcome),
                ..
            } => outcome
                .get("show_to_user")
                .and_then(serde_json::Value::as_bool),
            _ => None,
        })
        .collect();
    assert_eq!(choices, vec![true, false]);
    system.shutdown().await;
}

#[tokio::test]
async fn messages_interrupt_inference_and_resume_the_original_without_replaying_effects() {
    let provider = Arc::new(ReplayProvider::new(vec![
        MockReply::Calls(vec![AgentToolCall {
            id: "write-once".into(),
            name: "files__write_file".into(),
            arguments: json!({"path":"once.txt","content":"one effect","expected_sha256":null}),
        }]),
        MockReply::Pending,
        MockReply::Answer("Incoming message handled"),
        MockReply::Answer("Original request finished"),
    ]));
    let (directory, system) = runtime(provider.clone()).await;
    let original = enqueue_root(&system, "Original work");
    wait_requests(&provider, 2).await;
    let mut incoming = original.clone();
    incoming.id = uuid::Uuid::new_v4().to_string();
    incoming.source = MessageSource::Actor {
        actor_id: "core".into(),
    };
    incoming.in_reply_to = Some(original.id.clone());
    incoming.content = "New collaboration evidence".into();
    let incoming = system.store().enqueue(incoming, None).unwrap();
    system.wake("root");
    assert_eq!(
        wait_completed(&system, &incoming.id).await.reply.as_deref(),
        Some("Incoming message handled")
    );
    assert_eq!(
        wait_completed(&system, &original.id).await.reply.as_deref(),
        Some("Original request finished")
    );
    assert!(provider.dropped.load(Ordering::Acquire));
    {
        let requests = provider.requests.lock();
        assert!(requests[2].messages.iter().any(|row| matches!(row, AgentMessage::User { content } if content.as_text().contains("New collaboration evidence") && content.as_text().contains("Source: Actor core"))));
        assert!(requests[3].messages.iter().any(|row| matches!(row, AgentMessage::Assistant { content, .. } if content == "Incoming message handled")));
    }
    assert_eq!(
        std::fs::read_to_string(directory.path().join("workspace/once.txt")).unwrap(),
        "one effect"
    );
    assert_eq!(
        system
            .store()
            .tool_summaries("root", &original.id, None, 10)
            .unwrap()
            .items
            .len(),
        1
    );
    system.shutdown().await;
}

#[tokio::test]
async fn messages_are_retained_during_automatic_review_and_received_at_the_settled_boundary() {
    use crate::actors::ApprovalDecision;
    let outside = crate::sandbox::test_support::scoped_directory();
    let target = outside.path().join("approved.txt");
    let provider = Arc::new(ReplayProvider::new(vec![
        MockReply::Calls(vec![AgentToolCall {
            id: "waiting-write".into(),
            name: "files__write_file".into(),
            arguments: json!({"path":target,"content":"approved original effect","expected_sha256":null}),
        }]),
        MockReply::Answer("Incoming message received"),
        MockReply::Answer("Original write finished"),
    ]));
    provider.review_blocked.store(true, Ordering::Release);
    provider
        .review_replies
        .lock()
        .push_back(json!({"decision":"approve","reason":"requested output"}));
    let (_directory, system) = runtime(provider.clone()).await;
    system.install_local_core().unwrap();
    let original = system
        .send_user("root", "Write outside the workspace".into(), vec![], None)
        .unwrap();
    let approval = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(record) = system
                .store()
                .approvals(Some("root"), None, 10)
                .unwrap()
                .items
                .into_iter()
                .find(|record| record.decision == ApprovalDecision::Pending)
            {
                break record;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("filesystem operation never reached automatic review");
    let incoming = system
        .send_user("root", "Additional information".into(), vec![], None)
        .unwrap();
    assert!(system.store().has_queued_input("root").unwrap());
    assert!(!target.exists());
    assert_eq!(
        system.store().message(&original.id).unwrap().status,
        MessageStatus::Running
    );
    assert_eq!(
        system.store().approval(&approval.id).unwrap().decision,
        ApprovalDecision::Pending
    );
    provider.review_release.notify_one();
    let incoming = wait_completed(&system, &incoming.id).await;
    let original = wait_completed(&system, &original.id).await;
    assert_eq!(incoming.reply.as_deref(), Some("Incoming message received"));
    assert_eq!(original.reply.as_deref(), Some("Original write finished"));
    assert_eq!(
        std::fs::read_to_string(target).unwrap(),
        "approved original effect"
    );
    assert_eq!(
        system
            .store()
            .tool_summaries("root", &original.id, None, 10)
            .unwrap()
            .items
            .len(),
        1
    );
    assert_eq!(provider.review_requests.lock().len(), 1);
    system.shutdown().await;
}

#[tokio::test]
async fn message_tool_is_shared_and_related_messages_do_not_borrow_authority_or_reply_targets() {
    use crate::actors::ReplyTarget;
    let (_directory, system, _) = paused_runtime().await;
    let child = system
        .create_owned("root", project_config(&system, "collaborator"))
        .unwrap();
    let source = system
        .send_user("root", "Coordinate".into(), vec![], None)
        .unwrap();
    system.store().claim_next("root").unwrap().unwrap();
    let target = system
        .message_from_actor("root", &source.id, &child.id, "Investigate".into())
        .unwrap();
    let actor = system.actor("root").unwrap();
    let context = system
        .execution_context(&actor, &system.store().message(&source.id).unwrap())
        .unwrap();
    let mut apps =
        crate::builtin_apps::create_apps(&actor, system.clone(), &system.config()).unwrap();
    let result = apps.execute_tool(&AgentToolCall { id:"followup".into(), name:"actors__message".into(),
        arguments:json!({"stage_id":null,"actor_id":child.id,"reply_to":target.id,"content":"Keep this constraint"}) }, &context).await.unwrap();
    let followup = system
        .store()
        .message(result.payload["message_id"].as_str().unwrap())
        .unwrap();
    assert_eq!(followup.in_reply_to.as_deref(), Some(target.id.as_str()));
    assert_eq!(
        followup.reply_to,
        ReplyTarget::Actor {
            actor_id: "root".into(),
            request_id: source.id.clone()
        }
    );
    assert!(followup.remote_authority.is_none());
    let unrelated = system
        .send_user("core", "Another conversation".into(), vec![], None)
        .unwrap();
    assert!(apps.execute_tool(&AgentToolCall { id:"wrong-reference".into(), name:"actors__message".into(),
        arguments:json!({"stage_id":null,"actor_id":child.id,"reply_to":unrelated.id,"content":"Do not misattribute"}) }, &context).await.is_err());
    assert!(
        apps.execute_tool(
            &AgentToolCall {
                id: "retired".into(),
                name: "actors__steer".into(),
                arguments: json!({})
            },
            &context
        )
        .await
        .is_err()
    );
    system.shutdown().await;
}

struct BoundaryApp {
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
    effects: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait]
impl crate::app::App for BoundaryApp {
    fn id(&self) -> crate::app::AppId {
        crate::app::AppId::new("boundary").unwrap()
    }
    fn tool_specs(&self) -> Vec<crate::app::AppToolSpec> {
        vec![crate::app::AppToolSpec {
            name: "effect".into(),
            description: "Controlled test effect".into(),
            input_schema: crate::schema_utils::model_schema_for::<crate::app::EmptyAppArgs>(),
        }]
    }
    async fn execute_tool(
        &mut self,
        _call: &AgentToolCall,
        _context: &AppToolExecutionContext,
    ) -> Result<crate::app::AppToolExecutionResult> {
        let index = self.effects.fetch_add(1, Ordering::SeqCst);
        if index == 0 {
            self.entered.notify_one();
            self.release.notified().await;
        }
        Ok(crate::app::AppToolExecutionResult::new(
            "Effect completed",
            json!({"index":index}),
            None,
        ))
    }
}

#[tokio::test]
async fn messages_wait_for_the_whole_started_tool_group_and_never_split_its_observations() {
    let (_directory, system, provider) = paused_runtime().await;
    provider.replies.lock().extend([
        MockReply::Calls(vec![
            AgentToolCall {
                id: "first-effect".into(),
                name: "boundary__effect".into(),
                arguments: json!({}),
            },
            AgentToolCall {
                id: "second-effect".into(),
                name: "boundary__effect".into(),
                arguments: json!({}),
            },
        ]),
        MockReply::Answer("Incoming input processed"),
        MockReply::Answer("Original input processed"),
    ]);
    let original = system
        .send_user("root", "Execute both effects".into(), vec![], None)
        .unwrap();
    let original = system
        .store()
        .claim_next("root")
        .unwrap()
        .unwrap_or(original);
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let effects = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut apps = AppManager::new(vec![Box::new(BoundaryApp {
        entered: entered.clone(),
        release: release.clone(),
        effects: effects.clone(),
    })])
    .unwrap();
    let worker_system = system.clone();
    let active = original.clone();
    let execution = tokio::spawn(async move {
        let actor = worker_system.actor("root").unwrap();
        execute_message(
            &worker_system,
            &actor,
            &active,
            &ActorControl::default(),
            &mut apps,
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    let incoming = system
        .send_user("root", "Receive this after the group".into(), vec![], None)
        .unwrap();
    assert_eq!(
        system.store().message(&incoming.id).unwrap().status,
        MessageStatus::Queued
    );
    assert_eq!(provider.requests.lock().len(), 1);
    release.notify_one();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), execution)
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .as_deref(),
        Some("Original input processed")
    );
    assert_eq!(
        wait_completed(&system, &incoming.id).await.reply.as_deref(),
        Some("Incoming input processed")
    );
    assert_eq!(effects.load(Ordering::SeqCst), 2);
    {
        let requests = provider.requests.lock();
        let observations: Vec<_> = requests[1]
            .messages
            .iter()
            .filter_map(|row| match row {
                AgentMessage::Tool { tool_call_id, .. } => Some(tool_call_id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(observations, ["first-effect", "second-effect"]);
    }
    system.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn remote_automatic_review_retains_new_core_inputs_and_resumes_the_original_operation_once() {
    use crate::actors::ApprovalDecision;
    let outside = crate::sandbox::test_support::scoped_directory();
    let workspace = outside.path().join("new-workspace");
    let core_model = Arc::new(ReplayProvider::new(vec![]));
    let worker_model = Arc::new(ReplayProvider::new(vec![MockReply::PendingOnEveryRequest]));
    let ((_a, a, net_a), (_b, b, net_b), _relay) =
        device_pair(core_model.clone(), worker_model.clone()).await;
    let epoch = net_a.core.read().as_ref().unwrap().epoch();
    core_model.review_blocked.store(true, Ordering::Release);
    core_model
        .review_replies
        .lock()
        .push_back(json!({"decision":"approve","reason":"requested workspace"}));
    core_model.replies.lock().extend([
        MockReply::Calls(vec![AgentToolCall {id:"remote-create".into(),name:"actors__create".into(),
            arguments:json!({"stage_id":net_b.host.local,"name":"New collaborator","workspace":workspace,"instructions":null,"model":null})}]),
        MockReply::Answer("Incoming message received"),MockReply::Answer("Original creation completed"),MockReply::PendingOnEveryRequest,
    ]);
    let original = a
        .send_user("core", "Create the collaborator".into(), vec![], None)
        .unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if b.store()
                .approvals(Some("core"), None, 10)
                .unwrap()
                .items
                .iter()
                .any(|record| record.decision == ApprovalDecision::Pending)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("remote filesystem boundary did not reach automatic review");
    let incoming = a
        .send_user("core", "Additional information".into(), vec![], None)
        .unwrap();
    assert!(!workspace.exists());
    assert_eq!(
        a.store().message(&original.id).unwrap().status,
        MessageStatus::Running
    );
    assert!(a.store().message(&incoming.id).is_ok());
    core_model.review_release.notify_one();
    assert_eq!(
        wait_completed(&a, &incoming.id).await.reply.as_deref(),
        Some("Incoming message received")
    );
    assert_eq!(
        wait_completed(&a, &original.id).await.reply.as_deref(),
        Some("Original creation completed")
    );
    assert!(workspace.is_dir());
    assert_eq!(
        b.store()
            .actors()
            .unwrap()
            .iter()
            .filter(|actor| actor.config.name == "New collaborator")
            .count(),
        1
    );
    assert_eq!(core_model.review_requests.lock().len(), 1);
    assert!(worker_model.review_requests.lock().is_empty());
    assert_eq!(net_a.core.read().as_ref().unwrap().epoch(), epoch);
    net_a.stop().await;
    net_b.stop().await;
    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn remote_messages_preserve_correlation_and_deduplicate_the_exact_related_input() {
    use crate::stage::{ActorAddress, delivery::Deliveries};
    let source_model = Arc::new(ReplayProvider::new(vec![MockReply::PendingOnEveryRequest]));
    let target_model = Arc::new(ReplayProvider::new(vec![
        MockReply::Pending,
        MockReply::Answer("Followup handled"),
        MockReply::Answer("Original handled"),
    ]));
    let ((_a, a, net_a), (_b, b, net_b), _relay) =
        device_pair(source_model, target_model.clone()).await;
    let target = ActorAddress {
        stage_id: net_b.host.local,
        actor_id: "root".into(),
    };
    let original = a
        .send_stage_user(target.clone(), "Remote original".into(), "original")
        .unwrap();
    wait_requests(&target_model, 1).await;
    let source = a.core_device_actor().unwrap();
    let related = original.invocation_id.unwrap().to_string();
    let deliveries = Deliveries::new(a.store(), net_a.host.local);
    let now = chrono::Utc::now().timestamp_millis();
    let followup = deliveries
        .queue_related(
            &source,
            None,
            target.clone(),
            ("Remote followup".into(), vec![]),
            now,
            "followup",
            Some(&related),
        )
        .unwrap();
    let retry = deliveries
        .queue_related(
            &source,
            None,
            target.clone(),
            ("Remote followup".into(), vec![]),
            now,
            "followup",
            Some(&related),
        )
        .unwrap();
    assert_eq!(retry.invocation_id, followup.invocation_id);
    let changed = followup.invocation_id.unwrap().to_string();
    assert!(
        deliveries
            .queue_related(
                &source,
                None,
                target,
                ("Remote followup".into(), vec![]),
                now,
                "followup",
                Some(&changed)
            )
            .is_err()
    );
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let inputs = b.store().messages("root", None, 10).unwrap().items;
            if inputs.len() == 2
                && inputs
                    .iter()
                    .all(|input| input.status == MessageStatus::Completed)
            {
                let original = inputs
                    .iter()
                    .find(|input| input.content == "Remote original")
                    .unwrap();
                let followup = inputs
                    .iter()
                    .find(|input| input.content == "Remote followup")
                    .unwrap();
                assert_eq!(followup.in_reply_to.as_deref(), Some(original.id.as_str()));
                assert_eq!(original.reply.as_deref(), Some("Original handled"));
                assert_eq!(followup.reply.as_deref(), Some("Followup handled"));
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(target_model.dropped.load(Ordering::Acquire));
    net_a.stop().await;
    net_b.stop().await;
    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test]
async fn shutdown_preserves_both_suspended_inputs_and_does_not_replay_their_effects() {
    let provider = Arc::new(ReplayProvider::new(vec![
        MockReply::Calls(vec![AgentToolCall {
            id: "before-shutdown".into(),
            name: "files__write_file".into(),
            arguments: json!({"path":"retained.txt","content":"retained once","expected_sha256":null}),
        }]),
        MockReply::Pending,
        MockReply::Pending,
    ]));
    let (directory, system) = runtime(provider.clone()).await;
    let original = enqueue_root(&system, "Original suspended work");
    wait_requests(&provider, 2).await;
    let incoming = system
        .send_user("root", "Incoming unfinished work".into(), vec![], None)
        .unwrap();
    wait_requests(&provider, 3).await;
    system.shutdown().await;
    for input in [&original, &incoming] {
        assert_eq!(
            system.store().message(&input.id).unwrap().status,
            MessageStatus::Interrupted
        );
    }
    let replacement = Arc::new(ReplayProvider::new(vec![]));
    let reopened = ActorSystem::open_paused_with_factory(
        directory.path().join("data"),
        directory.path().join("workspace"),
        runtime_config(),
        Arc::new(ReplayFactory(replacement.clone())),
    )
    .await
    .unwrap();
    assert!(replacement.requests.lock().is_empty());
    assert_eq!(
        reopened
            .store()
            .tool_summaries("root", &original.id, None, 10)
            .unwrap()
            .items
            .len(),
        1
    );
    assert_eq!(
        std::fs::read_to_string(directory.path().join("workspace/retained.txt")).unwrap(),
        "retained once"
    );
    let history = reopened.store().history("root", 100).unwrap();
    for text in ["Original suspended work", "Incoming unfinished work"] {
        assert!(history.iter().any(
            |row| matches!(row,AgentMessage::User { content } if content.as_text().contains(text))
        ));
    }
    reopened.shutdown().await;
}
