use super::*;
use std::sync::atomic::AtomicUsize;

fn core_stop_context(system: &ActorSystem, template: &MessageRecord) -> AppToolExecutionContext {
    system.install_local_core().unwrap();
    let mut input = template.clone();
    input.id = uuid::Uuid::new_v4().to_string();
    input.seq = 0;
    input.actor_id = "core".into();
    input.status = MessageStatus::Queued;
    input.started_at_ms = None;
    input.finished_at_ms = None;
    input.cancel_requested = false;
    input.content = "Stop the hosted Actor".into();
    system.store().enqueue(input, None).unwrap();
    let input = system.store().claim_next("core").unwrap().unwrap();
    system
        .execution_context(&system.actor("core").unwrap(), &input)
        .unwrap()
}

#[tokio::test]
async fn interrupt_actor_repairs_all_nested_running_inputs() {
    let (_directory, system, _) = paused_runtime().await;
    let first = system
        .send_user("root", "Original work".into(), vec![], None)
        .unwrap();
    system.store().claim_next("root").unwrap().unwrap();
    let second = system
        .send_user("root", "First guidance".into(), vec![], None)
        .unwrap();
    system
        .store()
        .claim_incoming("root", &first.id)
        .unwrap()
        .unwrap();
    let third = system
        .send_user("root", "Second guidance".into(), vec![], None)
        .unwrap();
    system
        .store()
        .claim_incoming("root", &second.id)
        .unwrap()
        .unwrap();
    system
        .store()
        .interrupt_actor("root", "Worker stopped")
        .unwrap();
    for input in [&first, &second, &third] {
        assert_eq!(
            system.store().message(&input.id).unwrap().status,
            MessageStatus::Interrupted
        );
    }
    assert!(system.store().current_message("root").unwrap().is_none());
    system.shutdown().await;
}

#[tokio::test]
async fn core_stop_cancels_all_suspended_inputs_and_preserves_identity_history_and_future_work() {
    use crate::app::App;
    let provider = Arc::new(ReplayProvider::new(vec![
        MockReply::Calls(vec![AgentToolCall {
            id: "completed-once".into(),
            name: "files__write_file".into(),
            arguments: json!({"path":"once.txt","content":"one effect","expected_sha256":null}),
        }]),
        MockReply::PendingOnEveryRequest,
    ]));
    let (directory, system) = runtime(provider.clone()).await;
    let actor = system.actor("root").unwrap();
    let first = system
        .send_user("root", "Original work".into(), vec![], None)
        .unwrap();
    wait_requests(&provider, 2).await;
    let second = system
        .send_user("root", "First guidance".into(), vec![], None)
        .unwrap();
    wait_requests(&provider, 3).await;
    let third = system
        .send_user("root", "Second guidance".into(), vec![], None)
        .unwrap();
    wait_requests(&provider, 4).await;
    let context = core_stop_context(&system, &first);
    let mut app = crate::admin_actors_app::AdminActorsApp::new(system.clone());
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        app.execute_tool(
            &AgentToolCall {
                id: "stop-worker".into(),
                name: "stop".into(),
                arguments: json!({"stage_id":null,"actor_id":"root"}),
            },
            &context,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result.payload["cancelled_count"], 3);
    for input in [&first, &second, &third] {
        let state = system.store().message(&input.id).unwrap();
        assert_eq!(state.status, MessageStatus::Cancelled);
        assert!(state.cancel_requested);
    }
    assert_eq!(provider.pending.load(Ordering::Acquire), 0);
    assert_eq!(system.actor("root").unwrap().config, actor.config);
    assert_eq!(system.actor("root").unwrap().status, ActorStatus::Idle);
    assert!(
        !system
            .store()
            .history_page("root", 0, None, 100)
            .unwrap()
            .is_empty()
    );
    provider.replies.lock().clear();
    provider
        .replies
        .lock()
        .push_back(MockReply::Answer("New work completed"));
    let next = system
        .send_user("root", "Start new work".into(), vec![], None)
        .unwrap();
    assert_eq!(
        wait_completed(&system, &next.id).await.reply.as_deref(),
        Some("New work completed")
    );
    assert_eq!(
        std::fs::read_to_string(directory.path().join("workspace/once.txt")).unwrap(),
        "one effect"
    );
    assert_eq!(
        system
            .store()
            .tool_summaries("root", &first.id, None, 10)
            .unwrap()
            .items
            .len(),
        1
    );
    system.shutdown().await;
}

#[tokio::test]
async fn core_stop_rejects_self_readonly_foreign_and_ordinary_actor_authority() {
    use crate::app::App;
    let (_directory, system, _) = paused_runtime().await;
    let input = system
        .send_user("root", "Continue work".into(), vec![], None)
        .unwrap();
    let context = core_stop_context(&system, &input);
    let mut core = crate::admin_actors_app::AdminActorsApp::new(system.clone());
    let mut call = AgentToolCall {
        id: "stop".into(),
        name: "stop".into(),
        arguments: json!({"stage_id":null,"actor_id":"core"}),
    };
    assert!(core.execute_tool(&call, &context).await.is_err());
    let mut foreign = system.actor("root").unwrap();
    foreign.id = "foreign".into();
    foreign.parent_id = None;
    foreign.depth = 0;
    system.store().save_actor(&foreign).unwrap();
    system.store().stage_transaction(|tx| {
        tx.execute_batch("CREATE TABLE IF NOT EXISTS stage_account_actors(actor TEXT PRIMARY KEY REFERENCES actors(id) ON DELETE CASCADE,account TEXT NOT NULL);
            INSERT INTO stage_account_actors VALUES('core','owner-account'),('root','owner-account'),('foreign','other-account');")?;
        Ok(())
    }).unwrap();
    call.arguments["actor_id"] = json!("foreign");
    assert!(core.execute_tool(&call, &context).await.is_err());
    call.arguments["actor_id"] = json!("root");
    let mut readonly = context.clone();
    readonly.actor_call.as_mut().unwrap().remote_read_only = true;
    assert!(core.execute_tool(&call, &readonly).await.is_err());
    let input = system.store().claim_next("root").unwrap().unwrap();
    let actor = system.actor("root").unwrap();
    let mut ordinary = crate::actor_app::ActorApp::new("root".into(), system.clone());
    assert!(!ordinary.tool_specs().iter().any(|tool| tool.name == "stop"));
    let ordinary_context = system.execution_context(&actor, &input).unwrap();
    assert!(
        ordinary
            .execute_tool(&call, &ordinary_context)
            .await
            .is_err()
    );
    assert!(!system.store().message(&input.id).unwrap().cancel_requested);
    system.shutdown().await;
}

#[cfg(not(target_os = "android"))]
#[tokio::test]
async fn core_stop_reaps_running_terminal_and_discards_queued_input() {
    use crate::app::App;
    if !crate::sandbox::test_support::native_sandbox_available() {
        return;
    }
    let provider = Arc::new(ReplayProvider::new(vec![]));
    let (directory, system) = runtime(provider.clone()).await;
    let pidfile = directory.path().join("workspace/process.pid");
    let command = format!(
        "printf '%s' \"$$\" > '{}'; sleep 300",
        pidfile.display().to_string().replace('\'', "'\\''")
    );
    provider.replies.lock().extend([MockReply::Calls(vec![AgentToolCall {
        id:"long-process".into(),name:"terminal__terminal_exec".into(),
        arguments:json!({"command":command,"session_id":null,"workdir":null,"yield_time_ms":30000,"max_chars":1024}),
    }]),MockReply::PendingOnEveryRequest]);
    let first = system
        .send_user("root", "Run the process".into(), vec![], None)
        .unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        while !std::fs::read_to_string(&pidfile).is_ok_and(|v| v.parse::<u32>().is_ok()) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    #[cfg(not(target_os = "linux"))]
    let pid = std::fs::read_to_string(&pidfile)
        .unwrap()
        .parse::<u32>()
        .unwrap();
    let processes = sysinfo::System::new_all();
    #[cfg(not(target_os = "linux"))]
    let pids = std::collections::HashSet::from([sysinfo::Pid::from_u32(pid)]);
    #[cfg(target_os = "linux")]
    let pids = {
        // Bubblewrap's --unshare-pid makes $$ a namespace PID. Match the
        // unique fixture directory in the host supervisor's argv, then follow
        // host parent IDs so this checks the actual supervisor/descendant tree.
        let marker = directory.path().file_name().unwrap().to_string_lossy();
        let mut pids = processes
            .processes()
            .iter()
            .filter_map(|(pid, process)| {
                process
                    .cmd()
                    .iter()
                    .any(|arg| arg.to_string_lossy().contains(marker.as_ref()))
                    .then_some(*pid)
            })
            .collect::<std::collections::HashSet<_>>();
        assert!(
            !pids.is_empty(),
            "fixture has no observable host supervisor"
        );
        loop {
            let children = processes
                .processes()
                .iter()
                .filter_map(|(pid, process)| {
                    process
                        .parent()
                        .is_some_and(|parent| pids.contains(&parent))
                        .then_some(*pid)
                })
                .collect::<Vec<_>>();
            let before = pids.len();
            pids.extend(children);
            if pids.len() == before {
                break;
            }
        }
        pids
    };
    let instances = pids
        .into_iter()
        .map(|pid| {
            let started = processes
                .process(pid)
                .expect("fixture process exited before stop")
                .start_time();
            (pid, started)
        })
        .collect::<Vec<_>>();
    let queued = system
        .send_user("root", "Must not start".into(), vec![], None)
        .unwrap();
    let context = core_stop_context(&system, &first);
    let mut core = crate::admin_actors_app::AdminActorsApp::new(system.clone());
    core.execute_tool(
        &AgentToolCall {
            id: "stop-process".into(),
            name: "stop".into(),
            arguments: json!({"stage_id":null,"actor_id":"root"}),
        },
        &context,
    )
    .await
    .unwrap();
    assert_eq!(
        system.store().message(&first.id).unwrap().status,
        MessageStatus::Cancelled
    );
    let queued_state = system.store().message(&queued.id).unwrap();
    assert_eq!(queued_state.status, MessageStatus::Cancelled);
    assert!(queued_state.started_at_ms.is_none());
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let processes = sysinfo::System::new_all();
            if instances.iter().all(|(pid, started)| {
                processes
                    .process(*pid)
                    .is_none_or(|process| process.start_time() != *started)
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("stopped Actor retained its native process");
    assert_eq!(provider.requests.lock().len(), 1);
    system.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn core_stop_routes_to_remote_actor_and_cancels_all_its_open_inputs() {
    let core_model = Arc::new(ReplayProvider::new(vec![]));
    let worker_model = Arc::new(ReplayProvider::new(vec![MockReply::PendingOnEveryRequest]));
    let ((_a_dir, a, net_a), (_b_dir, b, net_b), _relay) =
        device_pair(core_model.clone(), worker_model.clone()).await;
    let first = b
        .send_user("root", "Remote original work".into(), vec![], None)
        .unwrap();
    wait_requests(&worker_model, 1).await;
    let second = b
        .send_user("root", "Remote guidance".into(), vec![], None)
        .unwrap();
    wait_requests(&worker_model, 2).await;
    let identity = b.actor("root").unwrap().config;
    let epoch = net_a.core.read().as_ref().unwrap().epoch();
    core_model.replies.lock().extend([
        MockReply::Calls(vec![AgentToolCall {
            id: "remote-stop".into(),
            name: "admin_actors__stop".into(),
            arguments: json!({"stage_id":net_b.host.local,"actor_id":"root"}),
        }]),
        MockReply::Answer("Remote Actor stopped"),
        MockReply::PendingOnEveryRequest,
    ]);
    let request = a
        .send_user("core", "Stop the remote Actor".into(), vec![], None)
        .unwrap();
    assert_eq!(
        wait_completed(&a, &request.id).await.reply.as_deref(),
        Some("Remote Actor stopped")
    );
    for id in [&first.id, &second.id] {
        assert_eq!(
            b.store().message(id).unwrap().status,
            MessageStatus::Cancelled
        );
    }
    assert_eq!(worker_model.pending.load(Ordering::Acquire), 0);
    assert_eq!(b.actor("root").unwrap().config, identity);
    assert_eq!(b.actor("root").unwrap().status, ActorStatus::Idle);
    assert_eq!(net_a.core.read().as_ref().unwrap().epoch(), epoch);
    let tools = a
        .store()
        .tool_summaries("core", &request.id, None, 10)
        .unwrap()
        .items;
    assert_eq!(tools.len(), 1);
    assert!(tools[0].error.is_none());
    assert_eq!(tools[0].outcome.as_ref().unwrap()["cancelled_count"], 2);
    assert_eq!(
        tools[0].outcome.as_ref().unwrap()["stage_id"],
        net_b.host.local.to_string()
    );
    net_a.stop().await;
    net_b.stop().await;
    a.shutdown().await;
    b.shutdown().await;
}

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
async fn actor_messages_retain_bodies_in_detail_and_only_references_in_events() {
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
        id: "send".into(),
        name: "actors__message".into(),
        arguments: json!({"stage_id":null,"actor_id":"root","reply_to":null,"content":"Coordinate"}),
    };
    assert!(apps.validate_tool_call(&call, &context).is_ok());
    for id in ["initial-delegation", "internal-followup"] {
        call.id = id.into();
        call.arguments["content"] = json!(id);
        let result = apps.execute_tool(&call, &context).await.unwrap();
        assert_eq!(result.payload["sent_message"]["content"], id);
        assert!(
            !result
                .model_content
                .as_ref()
                .unwrap()
                .contains("sent_message")
        );
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
    for summary in &summaries {
        assert!(summary.error.is_none(), "{:?}", summary.error);
        let outcome = summary.outcome.as_ref().unwrap();
        assert_eq!(outcome["sent_message"], json!(true));
        assert!(outcome.get("content").is_none());
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
                .get("sent_message")
                .and_then(serde_json::Value::as_bool),
            _ => None,
        })
        .collect();
    assert_eq!(choices, vec![true, true]);
    system.shutdown().await;
}

#[tokio::test]
async fn messages_interrupt_inference_and_resume_the_original_without_replaying_effects() {
    for reset in [false, true] {
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
        let original = enqueue_root(&system, "Investigate original work.");
        wait_requests(&provider, 2).await;
        let gate = Arc::new(tokio::sync::Notify::new());
        if reset {
            *provider.inference_gate.lock() = Some(gate.clone());
        }
        let mut incoming = original.clone();
        incoming.id = uuid::Uuid::new_v4().to_string();
        incoming.source = MessageSource::Actor {
            actor_id: "core".into(),
        };
        incoming.in_reply_to = Some(original.id.clone());
        incoming.content = "New collaboration evidence".into();
        let incoming = system.store().enqueue(incoming, None).unwrap();
        system.wake("root");
        if reset {
            wait_requests(&provider, 3).await;
            system
                .store()
                .reset_model_context("root", &incoming.id, "Collaboration evidence received; finish the incoming input, then resume the original task.")
                .unwrap();
            gate.notify_one();
        }
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
            if reset {
                let recovery = requests[3]
                    .messages
                    .iter()
                    .rev()
                    .find_map(|row| match row {
                        AgentMessage::User { content } => Some(content.as_text()),
                        _ => None,
                    })
                    .unwrap();
                assert!(recovery.contains(&original.id));
                assert!(
                    system
                        .store()
                        .history_page("root", 0, Some(&original.id), 100)
                        .unwrap()
                        .iter()
                        .any(|(_, _, row)| matches!(row, AgentMessage::User { content }
                        if content.as_text().contains(&original.content))),
                    "the restored locator must still resolve the original request"
                );
                assert!(!recovery.contains("New collaboration evidence"));
            } else {
                for pair in requests.windows(2) {
                    assert!(pair[1].messages.starts_with(&pair[0].messages));
                }
            }
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

#[test]
fn many_queued_steering_inputs_do_not_grow_the_worker_stack() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .thread_stack_size(1024 * 1024)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let (_directory, system, provider) = paused_runtime().await;
        const INCOMING: usize = 150;
        {
            let mut replies = provider.replies.lock();
            replies.push_back(MockReply::Calls(vec![AgentToolCall {
                id: "blocked-effect".into(),
                name: "boundary__effect".into(),
                arguments: json!({}),
            }]));
            for _ in 0..INCOMING {
                replies.push_back(MockReply::Answer("Incoming input processed"));
            }
            replies.push_back(MockReply::Answer("Original input processed"));
        }

        let original = system
            .send_user("root", "Run the blocked effect".into(), vec![], None)
            .unwrap();
        let message = system.store().claim_next("root").unwrap().unwrap();
        assert_eq!(message.id, original.id);
        let actor = system.execution_actor(&message).unwrap();
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let effects = Arc::new(AtomicUsize::new(0));
        let mut apps = AppManager::new(vec![Box::new(BoundaryApp {
            entered: entered.clone(),
            release: release.clone(),
            effects: effects.clone(),
        })])
        .unwrap();
        let worker_system = system.clone();
        let control = ActorControl::default();
        let execution = tokio::spawn(async move {
            execute_message(&worker_system, &actor, &message, &control, &mut apps).await
        });

        tokio::time::timeout(Duration::from_secs(5), entered.notified())
            .await
            .expect("the blocked effect did not start");
        let mut incoming = Vec::with_capacity(INCOMING);
        for index in 0..INCOMING {
            incoming.push(
                system
                    .send_user("root", format!("Incoming {index}"), vec![], None)
                    .unwrap(),
            );
        }
        release.notify_one();

        let reply = tokio::time::timeout(Duration::from_secs(120), execution)
            .await
            .expect("queued steering inputs stalled the worker")
            .unwrap()
            .unwrap();
        assert_eq!(reply.as_deref(), Some("Original input processed"));
        for item in incoming {
            assert_eq!(
                wait_completed(&system, &item.id).await.reply.as_deref(),
                Some("Incoming input processed")
            );
        }
        assert_eq!(effects.load(Ordering::SeqCst), 1);
        system.shutdown().await;
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn remote_core_initial_authorization_survives_new_inputs_without_replay() {
    let outside = crate::sandbox::test_support::scoped_directory();
    let workspace = outside.path().join("new-workspace");
    let core_model = Arc::new(ReplayProvider::new(vec![]));
    let worker_model = Arc::new(ReplayProvider::new(vec![MockReply::PendingOnEveryRequest]));
    let ((_a, a, net_a), (_b, b, net_b), _relay) =
        device_pair(core_model.clone(), worker_model.clone()).await;
    let epoch = net_a.core.read().as_ref().unwrap().epoch();
    let source_scopes = a.store().sandbox_scopes().unwrap();
    core_model.review_blocked.store(true, Ordering::Release);
    core_model
        .review_replies
        .lock()
        .push_back(json!({"decision":"approve","reason":"requested workspace"}));
    core_model.replies.lock().extend([
        MockReply::Calls(vec![AgentToolCall {id:"remote-create".into(),name:"actors__create".into(),
            arguments:json!({"stage_id":net_b.host.local,"name":"New collaborator","workspace":workspace,"instructions":null,"model":"mock","initial_authorization":null})}]),
        MockReply::Pending,
        MockReply::Answer("Incoming message received"),MockReply::Answer("Original creation completed"),MockReply::PendingOnEveryRequest,
    ]);
    let original = a
        .send_user("core", "Create the collaborator".into(), vec![], None)
        .unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if workspace.is_dir() && core_model.requests.lock().len() >= 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("Core did not finish remote creation before continuing inference");
    let incoming = a
        .send_user("core", "Additional information".into(), vec![], None)
        .unwrap();
    assert!(workspace.is_dir());
    assert_eq!(
        a.store().message(&original.id).unwrap().status,
        MessageStatus::Running
    );
    assert!(a.store().message(&incoming.id).is_ok());
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
    assert!(core_model.review_requests.lock().is_empty());
    assert!(
        b.store()
            .approvals(Some("core"), None, 10)
            .unwrap()
            .items
            .is_empty()
    );
    assert!(
        b.store()
            .sandbox_scopes()
            .unwrap()
            .iter()
            .any(|scope| scope.path == workspace.canonicalize().unwrap()
                && scope.write
                && scope.recursive)
    );
    assert_eq!(a.store().sandbox_scopes().unwrap(), source_scopes);
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
    let original = Deliveries::new(a.store(), net_a.host.local)
        .queue(
            &a.core_device_actor().unwrap(),
            None,
            target.clone(),
            ("Remote original".into(), vec![]),
            chrono::Utc::now().timestamp_millis(),
            "original",
        )
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn administrative_observation_and_model_changes_route_to_the_existing_remote_actor() {
    let coordinator = Arc::new(ReplayProvider::new(vec![]));
    let worker = Arc::new(ReplayProvider::new(vec![MockReply::PendingOnEveryRequest]));
    let ((_a_dir, a, net_a), (_b_dir, b, net_b), _relay) =
        device_pair(coordinator.clone(), worker.clone()).await;
    let original = b
        .send_user("root", "REMOTE_VISIBLE_PROGRESS".into(), vec![], None)
        .unwrap();
    wait_requests(&worker, 1).await;
    coordinator.replies.lock().extend([
        MockReply::Calls(vec![AgentToolCall{id:"inspect-remote-conversation".into(),name:"actors__read_conversation".into(),arguments:json!({"stage_id":net_b.host.local,"actor_id":"root","cursor":null,"limit":8})},
        AgentToolCall{id:"change-remote-model".into(),name:"admin_actors__set_model".into(),arguments:json!({"stage_id":net_b.host.local,"actor_id":"root","model":"mock"})}]),
        MockReply::Answer("Observed progress without messaging the worker"),
    ]);
    let request = a
        .send_user(
            "core",
            "Inspect progress and update its model".into(),
            vec![],
            None,
        )
        .unwrap();
    let done = wait_completed(&a, &request.id).await;
    assert_eq!(done.status, MessageStatus::Completed, "{:?}", done.error);
    assert_eq!(
        b.actor("root").unwrap().config.model.as_deref(),
        Some("mock")
    );
    assert_eq!(
        b.store().message(&original.id).unwrap().status,
        MessageStatus::Running
    );
    assert_eq!(
        worker.requests.lock().len(),
        1,
        "changing a preference cannot restart in-flight inference"
    );
    let calls = a
        .store()
        .tool_summaries("core", &request.id, None, 8)
        .unwrap()
        .items;
    assert_eq!(calls.len(), 2);
    assert!(calls.iter().all(|call| call.error.is_none()));
    let observed = calls
        .iter()
        .find(|call| call.name == "actors__read_conversation")
        .unwrap();
    let result = a.store().tool_result(&observed.id).unwrap();
    assert!(
        result.payload["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|bubble| bubble["content"] == "REMOTE_VISIBLE_PROGRESS")
    );
    assert_eq!(
        b.store().messages("root", None, 16).unwrap().items.len(),
        1,
        "observation must not create another input"
    );
    // A hosted worker can coordinate through the same read tools without using
    // Core's inbox or borrowing its execution authority.
    coordinator.replies.lock().extend([
        MockReply::Calls(vec![
            AgentToolCall {id:"worker-devices".into(),name:"actors__devices".into(),arguments:json!({})},
            AgentToolCall {id:"worker-observe".into(),name:"actors__read_conversation".into(),arguments:json!({"stage_id":net_b.host.local,"actor_id":"root","cursor":null,"limit":8})},
        ]),
        MockReply::Answer("Observed from a hosted worker"),
    ]);
    let hosted_input = a
        .send_user(
            "root",
            "Read the remote collaborator's progress".into(),
            vec![],
            None,
        )
        .unwrap();
    let done = wait_completed(&a, &hosted_input.id).await;
    assert_eq!(done.status, MessageStatus::Completed, "{:?}", done.error);
    let summaries = a
        .store()
        .tool_summaries("root", &hosted_input.id, None, 8)
        .unwrap()
        .items;
    assert_eq!(summaries.len(), 2);
    assert!(
        summaries.iter().all(|call| call.error.is_none()),
        "{summaries:?}"
    );
    let read = summaries
        .iter()
        .find(|call| call.name == "actors__read_conversation")
        .unwrap();
    let page = a.store().tool_result(&read.id).unwrap();
    assert!(
        page.payload["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|bubble| bubble["content"] == "REMOTE_VISIBLE_PROGRESS")
    );
    assert_eq!(
        worker.requests.lock().len(),
        1,
        "observation cannot interrupt the remote worker"
    );
    assert_eq!(b.store().messages("root", None, 16).unwrap().items.len(), 1);
    net_b.stop_abruptly().await;
    net_a.stop_abruptly().await;
    a.shutdown().await;
    b.shutdown().await;
}
