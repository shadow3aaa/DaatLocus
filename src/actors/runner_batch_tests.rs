use super::*;
use crate::actors::ReplyTarget;

#[tokio::test]
async fn live_progress_stays_in_one_execution_and_reviews_the_originating_task() {
    let (directory, system, provider) = paused_runtime().await;
    system.install_local_core().unwrap();
    system.set_workspace_backend_for_test(true);
    let parent = system
        .send_user("root", "Test branch writes".into(), vec![], None)
        .unwrap();
    system.store().claim_next("root").unwrap().unwrap();
    let worker = system
        .create_owned("root", project_config(&system, "Writer"))
        .unwrap();
    let observer = system
        .create_owned("root", project_config(&system, "Observer"))
        .unwrap();
    let original = system
        .message_from_actor(
            "root",
            &parent.id,
            &worker.id,
            "Write the beta branch report".into(),
        )
        .unwrap();
    let input = system.store().claim_next(&worker.id).unwrap().unwrap();
    let selected = directory.path().join("selected");
    std::fs::create_dir(&selected).unwrap();
    let target = selected.join("report.txt");
    const UPDATES: usize = 12;
    provider
        .replies
        .lock()
        .extend((0..UPDATES).map(|_| MockReply::Pending));
    provider.replies.lock().extend([
        MockReply::Calls(vec![
            AgentToolCall {
                id: "select-current-directory".into(),
                name: "workspace__select".into(),
                arguments: json!({"path":selected,"branch":"default"}),
            },
            AgentToolCall {
                id: "write-once-after-progress".into(),
                name: "files__write_file".into(),
                arguments: json!({"path":target,"content":"beta report"}),
            },
        ]),
        MockReply::Calls(vec![AgentToolCall {
            id: "finish-batch".into(),
            name: "actors__yield".into(),
            arguments: json!({}),
        }]),
    ]);
    provider
        .review_replies
        .lock()
        .push_back(json!({"decision":"approve","reason":"Requested report"}));
    let actor = system.execution_actor(&input).unwrap();
    let mut apps = builtin_apps::create_apps(&actor, system.clone(), &system.config()).unwrap();
    let control = ActorControl::default();
    let execution = execute_message(&system, &actor, &input, &control, &mut apps);
    let incoming = async {
        let mut previous = original.id.clone();
        let mut updates = Vec::new();
        for index in 0..UPDATES {
            wait_requests(&provider, index + 1).await;
            let mut update = original.clone();
            update.id = uuid::Uuid::new_v4().to_string();
            update.kind = MessageKind::Progress;
            update.source = MessageSource::Actor {
                actor_id: observer.id.clone(),
            };
            update.in_reply_to = Some(previous);
            update.content = format!("Observer update {index}: I am only inspecting default");
            previous = update.id.clone();
            updates.push(system.store().enqueue(update, None).unwrap());
        }
        updates
    };
    let (result, updates) = tokio::time::timeout(Duration::from_secs(30), async {
        tokio::join!(execution, incoming)
    })
    .await
    .unwrap();
    assert_eq!(result.unwrap(), None);
    system
        .store()
        .finish_message(&input.id, MessageStatus::Completed, None, None)
        .unwrap();
    assert_eq!(
        provider.requests.lock().len(),
        UPDATES + 2,
        "no per-message unwind inference"
    );
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "beta report");
    for update in updates {
        let settled = system.store().message(&update.id).unwrap();
        assert_eq!(settled.status, MessageStatus::Completed);
        assert_eq!(settled.reply, None);
        assert_eq!(settled.source, update.source);
    }
    {
        let reviews = provider.review_requests.lock();
        assert_eq!(reviews.len(), 1);
        let facts: serde_json::Value =
            serde_json::from_str(&reviews[0].current_user_message).unwrap();
        assert_eq!(facts["request"]["id"], original.id);
        assert_eq!(facts["task_request"]["id"], original.id);
        assert_eq!(facts["application"]["task"], original.content);
        assert_eq!(
            facts["actor"]["workspace"],
            json!(selected.canonicalize().unwrap())
        );
        assert_eq!(
            facts["request_inputs"].as_array().unwrap().len(),
            UPDATES + 1
        );
    }
    apps.shutdown().await.unwrap();
    system.shutdown().await;
}

#[tokio::test]
async fn batch_context_handoff_keeps_every_unfinished_input_locator() {
    let (_directory, system, provider) = paused_runtime().await;
    let first = system
        .send_user("root", "First unfinished task".into(), vec![], None)
        .unwrap();
    let second = system
        .send_user("root", "Second unfinished task".into(), vec![], None)
        .unwrap();
    let input = system.store().claim_next("root").unwrap().unwrap();
    system.store().claim_input_batch(&input.id).unwrap();
    let actor = system.execution_actor(&input).unwrap();
    let context = system.execution_context(&actor, &input).unwrap();
    let apps = builtin_apps::create_apps(&actor, system.clone(), &system.config()).unwrap();
    let tools = apps.model_tool_specs_for_context(&context).unwrap();
    let mut window = ModelContext::load(&system, &actor, &context, tools)
        .await
        .unwrap();
    let baseline = TokenEstimateBaseline::default();
    window
        .reload(&system, &input, provider.as_ref(), &baseline)
        .unwrap();
    let handoff = window
        .handoff_request(&input, provider.as_ref(), &baseline)
        .unwrap();
    let AgentMessage::User { content } = handoff.messages.last().unwrap() else {
        panic!("missing handoff");
    };
    assert!(content.as_text().contains(&first.id));
    assert!(content.as_text().contains(&second.id));
    assert!(handoff.tools.is_empty());
    apps.shutdown().await.unwrap();
    system.shutdown().await;
}

async fn advance_batch(system: &ActorSystem) -> MessageRecord {
    let input = system.store().claim_next("root").unwrap().unwrap();
    let actor = system.execution_actor(&input).unwrap();
    let mut apps = builtin_apps::create_apps(&actor, system.clone(), &system.config()).unwrap();
    let reply = execute_message(system, &actor, &input, &ActorControl::default(), &mut apps)
        .await
        .unwrap();
    system
        .store()
        .finish_message(&input.id, MessageStatus::Completed, reply.as_deref(), None)
        .unwrap();
    apps.shutdown().await.unwrap();
    system.store().message(&input.id).unwrap()
}

#[tokio::test]
async fn cancellation_backlog_uses_one_model_input_and_one_reply_with_separate_bubbles() {
    let (_directory, system, provider) = paused_runtime().await;
    let child = system
        .create_owned("root", project_config(&system, "Worker"))
        .unwrap();
    let owner = system
        .send_user("root", "Stop the delegated work".into(), vec![], None)
        .unwrap();
    system.store().claim_next("root").unwrap().unwrap();
    for index in 0..24 {
        let input = system
            .message_from_actor("root", &owner.id, &child.id, format!("Work item {index}"))
            .unwrap();
        system.cancel(&child.id, &input.id).unwrap();
    }
    system
        .store()
        .finish_message(&owner.id, MessageStatus::Completed, None, None)
        .unwrap();
    let before = system.store().messages("root", None, 100).unwrap().items;
    let backlog: Vec<_> = before
        .iter()
        .filter(|m| m.kind == MessageKind::Result)
        .cloned()
        .collect();
    assert_eq!(backlog.len(), 24);
    provider.replies.lock().push_back(MockReply::Answer(
        "All work has stopped; waiting for instructions.",
    ));
    advance_batch(&system).await;

    let messages = system.store().messages("root", None, 100).unwrap().items;
    assert_eq!(
        messages.len(),
        before.len(),
        "original bubbles must stay separate"
    );
    let replies: Vec<_> = messages
        .iter()
        .filter_map(|message| message.reply.as_deref())
        .collect();
    assert_eq!(replies, ["All work has stopped; waiting for instructions."]);
    for input in &backlog {
        let completed = system.store().message(&input.id).unwrap();
        assert_eq!(completed.status, MessageStatus::Completed);
        assert_eq!(completed.source, input.source);
        assert_eq!(completed.in_reply_to, input.in_reply_to);
        assert_eq!(completed.reply_to, input.reply_to);
        assert_eq!(completed.content, input.content);
        assert!(completed.input_batch.is_some());
        assert!(
            system
                .store()
                .has_history_for_message("root", &input.id)
                .unwrap()
        );
    }
    {
        let requests = provider.requests.lock();
        assert_eq!(
            requests.len(),
            1,
            "no recursive unwind or repeated acknowledgments"
        );
        let inputs: Vec<_> = requests[0]
            .messages
            .iter()
            .filter_map(|row| match row {
                AgentMessage::User { content } => Some(content),
                _ => None,
            })
            .collect();
        assert_eq!(inputs.len(), 1);
        for input in &backlog {
            assert!(inputs[0].as_text().contains(&input.id));
        }
    }
    system.shutdown().await;
}

#[tokio::test]
async fn queued_owner_messages_are_combined_without_losing_images_or_history() {
    let (_directory, system, provider) = paused_runtime().await;
    let png = base64::Engine::decode(&base64::engine::general_purpose::STANDARD,
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=").unwrap();
    let resource =
        crate::resources::publish(&system, "root", "inspect.png", "image/png", &png).unwrap();
    let image = crate::actors::Attachment {
        id: resource.id.clone(),
        name: resource.name.clone(),
        media_type: resource.media_type.clone(),
        size_bytes: resource.content.size,
        uri: resource.uri("root"),
    };
    let first = system
        .send_user(
            "root",
            "Inspect the first item".into(),
            vec![image.clone()],
            None,
        )
        .unwrap();
    let second = system
        .send_user(
            "root",
            "Also inspect the second item".into(),
            vec![image],
            None,
        )
        .unwrap();
    let third = system
        .send_user("root", "Give one combined answer".into(), vec![], None)
        .unwrap();
    provider
        .replies
        .lock()
        .push_back(MockReply::Answer("Combined answer"));
    advance_batch(&system).await;
    {
        let requests = provider.requests.lock();
        assert_eq!(requests.len(), 1);
        let inputs: Vec<_> = requests[0]
            .messages
            .iter()
            .filter_map(|row| match row {
                AgentMessage::User { content } => Some(content.as_text()),
                _ => None,
            })
            .collect();
        assert_eq!(inputs.len(), 1);
        assert_eq!(
            requests[0]
                .messages
                .iter()
                .filter_map(|row| match row {
                    AgentMessage::User { content } => Some(content.parts()),
                    _ => None,
                })
                .flatten()
                .filter(|part| matches!(part, AgentContentPart::ImageData { .. }))
                .count(),
            2
        );
        let mut position = 0;
        for input in [&first, &second, &third] {
            let next = inputs[0][position..].find(&input.content).unwrap() + position;
            position = next + input.content.len();
            assert_eq!(
                system.store().message(&input.id).unwrap().status,
                MessageStatus::Completed
            );
        }
        assert_eq!(
        system
            .store()
            .history("root", 100)
            .unwrap()
            .iter()
            .filter(|row| matches!(row, AgentMessage::User { content } if [&first, &second, &third].iter().any(|input| content.as_text().contains(&input.content))))
            .count(),
        3
    );
    }
    system.shutdown().await;
}

#[tokio::test]
async fn batching_never_borrows_owner_remote_authority_or_another_reply_target() {
    let (_directory, system, _provider) = paused_runtime().await;
    let first = system
        .send_user("root", "Owner request one".into(), vec![], None)
        .unwrap();
    let second = system
        .send_user("root", "Owner request two".into(), vec![], None)
        .unwrap();
    let mut actor_input = first.clone();
    actor_input.id = uuid::Uuid::new_v4().to_string();
    actor_input.source = MessageSource::Actor {
        actor_id: "worker".into(),
    };
    actor_input = system.store().enqueue(actor_input, None).unwrap();
    let mut remote = actor_input.clone();
    remote.id = uuid::Uuid::new_v4().to_string();
    remote.remote_authority = Some(crate::actors::RemoteAuthority {
        read_only: true,
        peer: crate::stage::StageId::new(),
        origin_actor: "worker".into(),
        reply_for: None,
    });
    remote = system.store().enqueue(remote, None).unwrap();
    let mut different_reply = actor_input.clone();
    different_reply.id = uuid::Uuid::new_v4().to_string();
    different_reply.reply_to = ReplyTarget::Actor {
        actor_id: "worker".into(),
        request_id: "separate-request".into(),
    };
    different_reply = system.store().enqueue(different_reply, None).unwrap();

    system.store().claim_next("root").unwrap().unwrap();
    let batch = system.store().claim_input_batch(&first.id).unwrap();
    assert_eq!(
        batch.iter().map(|input| &input.id).collect::<Vec<_>>(),
        [&first.id, &second.id]
    );
    for input in [&actor_input, &remote, &different_reply] {
        assert_eq!(
            system.store().message(&input.id).unwrap().status,
            MessageStatus::Queued
        );
        assert!(
            system
                .store()
                .message(&input.id)
                .unwrap()
                .input_batch
                .is_none()
        );
    }
    system
        .store()
        .finish_message(&first.id, MessageStatus::Completed, None, None)
        .unwrap();
    system.store().claim_next("root").unwrap().unwrap();
    assert_eq!(
        system
            .store()
            .claim_input_batch(&actor_input.id)
            .unwrap()
            .len(),
        1
    );
    assert!(
        system
            .store()
            .active_batch_input(&actor_input.id)
            .unwrap()
            .remote_authority
            .is_none()
    );
    system
        .store()
        .finish_message(&actor_input.id, MessageStatus::Completed, None, None)
        .unwrap();
    system.store().claim_next("root").unwrap().unwrap();
    assert_eq!(
        system.store().claim_input_batch(&remote.id).unwrap().len(),
        1
    );
    system.shutdown().await;
}

#[tokio::test]
async fn cancelling_one_batched_input_preserves_other_inputs_and_completed_effects() {
    let (_directory, system, provider) = paused_runtime().await;
    let first = system
        .send_user("root", "Write the first result".into(), vec![], None)
        .unwrap();
    let second = system
        .send_user(
            "root",
            "Then inspect the remaining result".into(),
            vec![],
            None,
        )
        .unwrap();
    provider.replies.lock().extend([
        MockReply::Calls(vec![AgentToolCall {
            id: "write-once".into(),
            name: "files__write_file".into(),
            arguments: json!({"path":"once.txt","content":"ONE EFFECT"}),
        }]),
        MockReply::Pending,
        MockReply::Answer("The remaining input is complete."),
    ]);
    let cancellation = async {
        tokio::time::timeout(Duration::from_secs(20), async {
            while provider.requests.lock().len() < 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        system.cancel("root", &first.id).unwrap();
    };
    let (cancelled, ()) = tokio::join!(advance_batch(&system), cancellation);
    assert_eq!(cancelled.status, MessageStatus::Cancelled);
    let remaining = system.store().message(&second.id).unwrap();
    assert_eq!(remaining.status, MessageStatus::Completed);
    assert_eq!(
        remaining.reply.as_deref(),
        Some("The remaining input is complete.")
    );
    assert_eq!(
        std::fs::read_to_string(
            system
                .actor("root")
                .unwrap()
                .config
                .workspace
                .join("once.txt")
        )
        .unwrap(),
        "ONE EFFECT"
    );
    assert_eq!(
        provider.requests.lock().len(),
        3,
        "resume inference without replaying effects"
    );
    assert!(provider.dropped.load(Ordering::Acquire));
    assert_eq!(
        system
            .store()
            .tool_summaries("root", &first.id, None, 100)
            .unwrap()
            .items
            .len(),
        1
    );
    assert_eq!(
        system
            .store()
            .tool_summaries("root", &second.id, None, 100)
            .unwrap()
            .items
            .len(),
        0
    );
    system.shutdown().await;
}
