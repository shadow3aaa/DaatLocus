use super::*;
use crate::actors::ReplyTarget;

async fn advance_inbox(system: &ActorSystem, actor_id: &str) -> MessageRecord {
    let input = system.store().claim_next(actor_id).unwrap().unwrap();
    let actor = system.execution_actor(&input).unwrap();
    let mut apps = builtin_apps::create_apps(&actor, system.clone(), &system.config()).unwrap();
    let control = ActorControl::default();
    let result = execute_message(system, &actor, &input, &control, &mut apps).await;
    finish_input(system, &input, &control, result).unwrap();
    apps.shutdown().await.unwrap();
    system.store().message(&input.id).unwrap()
}

#[tokio::test]
async fn messages_during_inference_wait_for_tools_then_settle_without_unwinding_reply_targets() {
    let (_directory, system, provider) = paused_runtime().await;
    let parent = enqueue_root(&system, "Coordinate the work");
    system.store().claim_next("root").unwrap().unwrap();
    let worker = system
        .create_owned("root", project_config(&system, "Writer"))
        .unwrap();
    let original = system
        .message_from_actor("root", &parent.id, &worker.id, "Write both files".into())
        .unwrap();
    let gate = Arc::new(tokio::sync::Notify::new());
    *provider.inference_gate.lock() = Some(gate.clone());
    provider.replies.lock().extend([
        MockReply::Calls(
            ["first", "second"]
                .map(|name| AgentToolCall {
                    id: name.into(),
                    name: "files__write_file".into(),
                    arguments: json!({"path":format!("{name}.txt"),"content":name}),
                })
                .into(),
        ),
        MockReply::Answer(""),
    ]);
    let execution = advance_inbox(&system, &worker.id);
    let incoming = async {
        wait_requests(&provider, 1).await;
        let mut updates = Vec::new();
        for index in 0..2 {
            let parent = system
                .test_actor_input("root", format!("Parent input {index}"), vec![], None)
                .unwrap();
            let mut update = original.clone();
            update.id = uuid::Uuid::new_v4().to_string();
            update.seq = 0;
            update.kind = MessageKind::Progress;
            update.reply_to = ReplyTarget::Actor {
                actor_id: "root".into(),
                request_id: parent.id,
            };
            update.content = format!("Buffered collaboration {index}");
            updates.push(system.store().enqueue(update, None).unwrap());
        }
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(provider.requests.lock().len(), 1);
        for update in &updates {
            assert_eq!(
                system.store().message(&update.id).unwrap().status,
                MessageStatus::Queued
            );
        }
        gate.notify_one();
        updates
    };
    let (finished, updates) = tokio::time::timeout(Duration::from_secs(15), async {
        tokio::join!(execution, incoming)
    })
    .await
    .unwrap();
    assert_eq!(finished.status, MessageStatus::Completed);
    assert_eq!(finished.reply, None);
    for update in &updates {
        assert_eq!(
            system.store().message(&update.id).unwrap().status,
            MessageStatus::Completed
        );
    }
    assert!(system.store().claim_next(&worker.id).unwrap().is_none());
    {
        let requests = provider.requests.lock();
        assert_eq!(
            requests.len(),
            2,
            "one continuation for the whole inbox, with no old-input unwind"
        );
        let rows = &requests[1].messages;
        let last_tool = rows
            .iter()
            .rposition(
                |r| matches!(r, AgentMessage::Tool {tool_call_id,..} if tool_call_id=="second"),
            )
            .unwrap();
        for update in &updates {
            assert!(rows[last_tool+1..].iter().any(|r| matches!(r,AgentMessage::User{content} if content.as_text().contains(&update.content))));
        }
        for name in ["first", "second"] {
            assert_eq!(
                std::fs::read_to_string(worker.config.workspace.join(format!("{name}.txt")))
                    .unwrap(),
                name
            );
        }
    }
    let root = system.store().messages("root", None, 20).unwrap().items;
    let notices: Vec<_> = root
        .iter()
        .filter(|message| message.kind == MessageKind::Result)
        .collect();
    assert_eq!(
        root.len(),
        4,
        "natural completion sends no final text, only a runtime notice"
    );
    assert_eq!(notices.len(), 1, "coalesced inputs share one notice");
    assert!(notices[0].content.contains("without replying"));
    assert_eq!(notices[0].in_reply_to.as_deref(), Some(parent.id.as_str()));
    system.shutdown().await;
}

#[tokio::test]
async fn messages_during_a_final_response_are_received_when_the_actor_becomes_idle() {
    let (_directory, system, provider) = paused_runtime().await;
    let first = enqueue_root(&system, "First work");
    let gate = Arc::new(tokio::sync::Notify::new());
    *provider.inference_gate.lock() = Some(gate.clone());
    provider.replies.lock().extend([
        MockReply::Answer("First finished"),
        MockReply::Answer("Second finished"),
    ]);
    let execution = advance_inbox(&system, "root");
    let incoming = async {
        wait_requests(&provider, 1).await;
        let next = enqueue_root(&system, "Second work");
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(provider.requests.lock().len(), 1);
        gate.notify_one();
        next
    };
    let (finished, next) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(execution, incoming)
    })
    .await
    .unwrap();
    assert_eq!(finished.id, first.id);
    assert_eq!(finished.reply.as_deref(), Some("First finished"));
    assert_eq!(
        system.store().message(&next.id).unwrap().status,
        MessageStatus::Queued
    );
    assert_eq!(advance_inbox(&system, "root").await.id, next.id);
    assert_eq!(provider.requests.lock().len(), 2);
    system.shutdown().await;
}

#[tokio::test]
async fn explicit_reply_does_not_end_execution_and_natural_completion_adds_no_second_delivery() {
    let (_directory, system, provider) = paused_runtime().await;
    let parent = enqueue_root(&system, "Delegate work");
    system.store().claim_next("root").unwrap().unwrap();
    let worker = system
        .create_owned("root", project_config(&system, "Writer"))
        .unwrap();
    system
        .message_from_actor(
            "root",
            &parent.id,
            &worker.id,
            "Send the useful result".into(),
        )
        .unwrap();
    provider.replies.lock().extend([
        MockReply::Calls(vec![
            AgentToolCall {
                id: "send".into(),
                name: "actors__reply".into(),
                arguments: json!({"content":"Useful result"}),
            },
            AgentToolCall {
                id: "write".into(),
                name: "files__write_file".into(),
                arguments: json!({"path":"after-reply.txt","content":"still working"}),
            },
        ]),
        MockReply::Answer(""),
    ]);
    assert_eq!(
        advance_inbox(&system, &worker.id).await.status,
        MessageStatus::Completed
    );
    assert_eq!(
        std::fs::read_to_string(worker.config.workspace.join("after-reply.txt")).unwrap(),
        "still working"
    );
    let messages = system.store().messages("root", None, 20).unwrap().items;
    assert_eq!(messages.len(), 2);
    assert_eq!(
        messages
            .iter()
            .filter(|m| m.content == "Useful result")
            .count(),
        1
    );
    assert_eq!(provider.requests.lock().len(), 2);
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
        .test_actor_input(
            "root",
            "Inspect the first item".into(),
            vec![image.clone()],
            None,
        )
        .unwrap();
    let second = system
        .test_actor_input(
            "root",
            "Also inspect the second item".into(),
            vec![image],
            None,
        )
        .unwrap();
    let third = system
        .test_actor_input("root", "Give one combined answer".into(), vec![], None)
        .unwrap();
    provider
        .replies
        .lock()
        .push_back(MockReply::Answer("Combined answer"));
    advance_inbox(&system, "root").await;
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
async fn cancelling_one_received_input_preserves_other_inputs_and_completed_effects() {
    let (_directory, system, provider) = paused_runtime().await;
    let first = system
        .test_actor_input("root", "Write the first result".into(), vec![], None)
        .unwrap();
    let second = system
        .test_actor_input(
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
    let (cancelled, ()) = tokio::join!(advance_inbox(&system, "root"), cancellation);
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

#[tokio::test]
async fn context_handoff_keeps_every_unfinished_input_locator() {
    let (_directory, system, provider) = paused_runtime().await;
    let first = system
        .test_actor_input("root", "First unfinished task".into(), vec![], None)
        .unwrap();
    let second = system
        .test_actor_input("root", "Second unfinished task".into(), vec![], None)
        .unwrap();
    let input = system.store().claim_next("root").unwrap().unwrap();
    system.store().receive_inputs(&input.id).unwrap();
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

#[tokio::test]
async fn buffered_inputs_keep_hard_authorities_separate_without_using_reply_targets_as_boundaries()
{
    use crate::actors::{MessageSource, RemoteAuthority};
    let (_directory, system, _) = paused_runtime().await;
    let owner = enqueue_root(&system, "Owner authority");
    system.store().claim_next("root").unwrap().unwrap();
    let mut collaboration = owner.clone();
    collaboration.id = uuid::Uuid::new_v4().to_string();
    collaboration.source = MessageSource::Actor {
        actor_id: "root".into(),
    };
    let collaboration = system.store().enqueue(collaboration, None).unwrap();
    let mut remote = owner.clone();
    remote.id = uuid::Uuid::new_v4().to_string();
    remote.remote_authority = Some(RemoteAuthority {
        read_only: true,
        peer: crate::stage::StageId::new(),
        origin_actor: "root".into(),
        reply_for: None,
    });
    let remote = system.store().enqueue(remote, None).unwrap();
    let local = enqueue_root(&system, "Local authority after remote input");
    assert_eq!(system.store().active_input(&owner.id).unwrap().id, owner.id);
    let received = system.store().receive_inputs(&owner.id).unwrap();
    assert_eq!(
        received.iter().map(|m| &m.id).collect::<Vec<_>>(),
        [&owner.id, &collaboration.id]
    );
    system
        .store()
        .finish_execution(&owner.id, MessageStatus::Completed, None, None)
        .unwrap();

    let remote_active = system.store().claim_next("root").unwrap().unwrap();
    assert_eq!(remote_active.id, remote.id);
    assert_eq!(
        system
            .store()
            .receive_inputs(&remote.id)
            .unwrap()
            .iter()
            .map(|m| &m.id)
            .collect::<Vec<_>>(),
        [&remote.id]
    );
    system
        .store()
        .finish_execution(&remote.id, MessageStatus::Completed, None, None)
        .unwrap();

    let local_active = system.store().claim_next("root").unwrap().unwrap();
    assert_eq!(local_active.id, local.id);
    system
        .store()
        .finish_execution(&local.id, MessageStatus::Completed, None, None)
        .unwrap();
    system.shutdown().await;
}
