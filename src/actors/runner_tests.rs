#[path = "runner_inbox_tests.rs"]
mod inbox_tests;
#[path = "runner_model_wait_tests.rs"]
mod model_wait_tests;
#[path = "super_skills_tests.rs"]
mod super_skills_tests;
#[path = "tool_definition_tests.rs"]
mod tool_definition_tests;
#[path = "runner_workspace_tests.rs"]
mod workspace_tests;

use super::super::{
    MessageKind, MessageSource, ModelFactory, model_context::balanced_history, prompt::SystemPrompt,
};
use super::*;
use crate::{
    context_budget::{RequestBudgetLimits, estimate_agent_turn_request},
    core::TokenUsageInfo,
    model_protocol::{AgentContentPart, AgentToolSpec, PromptRequest},
};
use std::{collections::VecDeque, sync::atomic::AtomicBool};

async fn wait_requests(provider: &ReplayProvider, count: usize) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while provider.requests.lock().len() < count {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("incoming message did not reach inference");
}

enum MockReply {
    Answer(&'static str),
    AnswerOnEveryRequest(&'static str),
    Calls(Vec<AgentToolCall>),
    Gated(Box<MockReply>, Arc<tokio::sync::Notify>),
    Pending,
    // An explicitly unresponsive provider may resume inference any number of
    // times. Unlike Calls, this tail is never consumed or replayed as an effect.
    PendingOnEveryRequest,
    FollowUp(&'static str),
    ReasoningOnly,
    MalformedEmpty,
    Failure(&'static str),
}

#[tokio::test]
async fn model_preferences_switch_at_the_next_inference_without_restarting_work_or_replaying_tools()
{
    let previous = Arc::new(ReplayProvider::new(vec![MockReply::Calls(vec![
        AgentToolCall {
            id: "one-write".into(),
            name: "files__write_file".into(),
            arguments: json!({"path":"once.txt","content":"ONE_EFFECT"}),
        },
    ])]));
    let next = Arc::new(ReplayProvider::new(vec![MockReply::Answer(
        "Completed with the next model",
    )]));
    let gate = Arc::new(tokio::sync::Notify::new());
    *previous.inference_gate.lock() = Some(gate.clone());
    let dir = crate::sandbox::test_support::scoped_directory();
    let mut config = runtime_config();
    config
        .models
        .insert("worker".into(), config.models["mock"].clone());
    config.models.get_mut("mock").unwrap().supports_vision = Some(false);
    config.models.get_mut("worker").unwrap().supports_vision = Some(true);
    let system = ActorSystem::open_with_factory(
        dir.path().join("data"),
        dir.path().join("work"),
        config,
        Arc::new(ModelSwitchFactory {
            core: previous.clone(),
            worker: next.clone(),
        }),
    )
    .await
    .unwrap();
    system.seed_test_actor("root");
    let before = system.actor("root").unwrap();
    let input = enqueue_root(&system, "Finish this exact request");
    previous.started.notified().await;
    let core_input = system
        .send_user("core", "Change the worker model".into(), vec![], None)
        .unwrap();
    system.store().claim_next("core").unwrap().unwrap();
    let caller = crate::app::ActorCallContext {
        remote_read_only: false,
        actor_id: "core".into(),
        message_id: core_input.id,
        device_policy: None,
    };
    let invalid = system
        .set_model_from_core("root", Some("missing".into()), &caller)
        .await;
    assert!(invalid.is_err());
    system
        .set_model_from_core("root", Some("worker".into()), &caller)
        .await
        .unwrap();
    assert_eq!(
        system.store().message(&input.id).unwrap().status,
        MessageStatus::Running
    );
    assert_eq!(previous.requests.lock().len(), 1);
    assert_eq!(next.requests.lock().len(), 0);
    gate.notify_one();
    let finished = wait_completed(&system, &input.id).await;
    assert_eq!(
        finished.status,
        MessageStatus::Completed,
        "{:?}",
        finished.error
    );
    assert_eq!(
        finished.reply.as_deref(),
        Some("Completed with the next model")
    );
    assert_eq!(previous.requests.lock().len(), 1);
    assert_eq!(next.requests.lock().len(), 1);
    assert!(
        next.requests.lock()[0].messages.iter().any(
            |row| matches!(row,AgentMessage::Tool{tool_call_id,..} if tool_call_id=="one-write")
        )
    );
    assert_eq!(
        system
            .store()
            .tool_summaries("root", &input.id, None, 8)
            .unwrap()
            .items
            .len(),
        1
    );
    assert_eq!(
        std::fs::read_to_string(before.config.workspace.join("once.txt")).unwrap(),
        "ONE_EFFECT"
    );
    assert_eq!(
        system.actor("root").unwrap().created_at_ms,
        before.created_at_ms
    );
    assert!(
        !previous.requests.lock()[0]
            .tools
            .iter()
            .any(|tool| tool.name == "files__view_image")
    );
    assert!(
        next.requests.lock()[0]
            .tools
            .iter()
            .any(|tool| tool.name == "files__view_image")
    );
    system.shutdown().await;
}

#[tokio::test]
async fn instruction_edits_do_not_interrupt_inference_and_apply_to_the_next_model_turn() {
    let provider = Arc::new(ReplayProvider::new(vec![
        MockReply::Calls(vec![AgentToolCall {
            id: "inspect".into(),
            name: "files__read_file".into(),
            arguments: json!({"path":"check.txt","start_line":null,"line_count":null}),
        }]),
        MockReply::Answer("Finished without replay"),
    ]));
    let gate = Arc::new(tokio::sync::Notify::new());
    *provider.inference_gate.lock() = Some(gate.clone());
    let (_directory, system) = runtime(provider.clone()).await;
    let old = system.actor("root").unwrap();
    std::fs::write(old.config.workspace.join("check.txt"), "An existing file").unwrap();
    let input = system
        .test_actor_input("root", "Inspect the workspace".into(), vec![], None)
        .unwrap();
    provider.started.notified().await;
    let file = system
        .instructions()
        .save(None, None, "hot", "UNIQUE HOT INSTRUCTIONS".into())
        .unwrap();
    system
        .instructions()
        .replace(
            crate::instruction_layers::Role::Others,
            vec![file.id],
            |_| Ok(()),
        )
        .unwrap();
    assert_eq!(
        provider.requests.lock().len(),
        1,
        "editing cannot cancel or restart in-flight inference"
    );
    assert_eq!(
        system.store().message(&input.id).unwrap().status,
        MessageStatus::Running
    );
    gate.notify_one();
    let finished = wait_completed(&system, &input.id).await;
    assert_eq!(
        finished.status,
        MessageStatus::Completed,
        "{:?}",
        finished.error
    );
    {
        let requests = provider.requests.lock();
        assert_eq!(requests.len(), 2);
        assert!(
            !serde_json::to_string(&requests[0].messages[0])
                .unwrap()
                .contains("UNIQUE HOT INSTRUCTIONS")
        );
        assert!(
            serde_json::to_string(&requests[1].messages[0])
                .unwrap()
                .contains("UNIQUE HOT INSTRUCTIONS")
        );
    }
    assert_eq!(
        system
            .store()
            .tool_summaries("root", &input.id, None, 10)
            .unwrap()
            .items
            .len(),
        1
    );
    assert_eq!(
        system.actor("root").unwrap().created_at_ms,
        old.created_at_ms
    );
    system.shutdown().await;
}

#[tokio::test]
async fn approval_rejection_is_not_a_tool_or_input_failure() {
    let outside = crate::sandbox::test_support::scoped_directory();
    let target = outside.path().join("not-authorized.txt");
    let reason = "The requested output folder is not authorized <private>";
    let provider = Arc::new(ReplayProvider::new(vec![
        MockReply::Calls(vec![AgentToolCall {
            id: "rejected-write".into(),
            name: "files__write_file".into(),
            arguments: json!({"path":target,"content":"must not be written"}),
        }]),
        MockReply::Answer("I will use an authorized alternative."),
    ]));
    provider
        .review_replies
        .lock()
        .push_back(json!({"decision":"deny","reason":reason}));
    let (_dir, system) = runtime(provider.clone()).await;
    let input = system
        .test_actor_input("root", "Create the output".into(), vec![], None)
        .unwrap();

    let finished = wait_completed(&system, &input.id).await;
    assert_eq!(
        finished.status,
        MessageStatus::Completed,
        "{:?}",
        finished.error
    );
    assert!(finished.error.is_none());
    assert!(!target.exists(), "rejection must prevent the effect");
    assert!(
        !system
            .store()
            .sandbox_scopes()
            .unwrap()
            .iter()
            .any(|scope| scope.write && target.starts_with(&scope.path))
    );
    let record = system
        .store()
        .approvals(Some("root"), None, 10)
        .unwrap()
        .items
        .pop()
        .unwrap();
    let tools = system
        .store()
        .tool_summaries("root", &input.id, None, 10)
        .unwrap()
        .items;
    assert_eq!(tools.len(), 1);
    assert!(tools.iter().all(|tool| tool.error.is_none()));
    let original = system.store().tool_result(&tools[0].id).unwrap();
    assert!(
        original.started_at_ms.is_some(),
        "the tool entered its original invocation and suspended at the filesystem boundary"
    );
    let tool = &tools[0];
    let expected = json!({"approval":{"decision":"denied","approval_id":record.id,
        "stage_id":system.store().public_stage_id().unwrap().to_string(),"reason":reason}});
    assert!(tool.error.is_none());
    assert_eq!(tool.outcome.as_ref(), Some(&expected));
    let detail = system.store().tool_result(&tool.id).unwrap();
    assert!(detail.error.is_none());
    assert_eq!(detail.payload, expected);
    let events = system.store().events(0, Some("root"), false, 100).unwrap();
    assert!(events.iter().any(|event| matches!(&event.event,
        ActorEvent::ToolFinished {call_id, error:None, outcome:Some(outcome), ..}
            if call_id == "rejected-write" && outcome == &expected)));
    {
        let requests = provider.requests.lock();
        assert_eq!(requests.len(), 2);
        assert!(requests[1].messages.iter().any(|row| matches!(row,
            AgentMessage::Tool {content,..} if content.contains(&record.id)
                && content.contains(reason) && !content.starts_with("App error:"))));
    }
    system.shutdown().await;
}

#[tokio::test]
async fn filesystem_boundary_approval_resumes_original_call_without_a_model_retry() {
    let outside = crate::sandbox::test_support::scoped_directory();
    let target = outside.path().join("approved-output.txt");
    let provider = Arc::new(ReplayProvider::new(vec![
        MockReply::Calls(vec![AgentToolCall {
            id: "original-write".into(),
            name: "files__write_file".into(),
            arguments: json!({"path":target,"content":"original call output"}),
        }]),
        MockReply::Answer("Output created"),
    ]));
    provider
        .review_replies
        .lock()
        .push_back(json!({"decision":"approve","reason":"Requested output"}));
    let (_dir, system) = runtime(provider.clone()).await;
    system.suspend_execution().await.unwrap();
    let task = system
        .test_actor_input("root", "Create the output".into(), vec![], None)
        .unwrap();
    system.store().claim_next("root").unwrap().unwrap();
    system
        .store()
        .finish_message(&task.id, MessageStatus::Completed, None, None)
        .unwrap();
    let mut update = task.clone();
    update.id = uuid::Uuid::new_v4().to_string();
    update.kind = MessageKind::Progress;
    update.source = MessageSource::Actor {
        actor_id: "core".into(),
    };
    update.in_reply_to = Some(task.id.clone());
    update.content = "My inspection is complete".into();
    let input = system.store().enqueue(update, None).unwrap();
    system.resume_execution().await.unwrap();

    let finished = wait_completed(&system, &input.id).await;
    assert_eq!(
        finished.status,
        MessageStatus::Completed,
        "{:?}",
        finished.error
    );
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "original call output"
    );
    assert_eq!(
        system
            .store()
            .tool_summaries("root", &input.id, None, 10)
            .unwrap()
            .items
            .len(),
        1
    );
    assert_eq!(
        system
            .store()
            .approvals(Some("root"), None, 10)
            .unwrap()
            .items
            .len(),
        1
    );
    assert!(!system.store().sandbox_scopes().unwrap().is_empty());
    {
        let reviews = provider.review_requests.lock();
        let facts: serde_json::Value =
            serde_json::from_str(&reviews[0].current_user_message).unwrap();
        assert_eq!(facts["request"]["id"], input.id);
        assert_eq!(facts["task_request"]["id"], task.id);
        assert_eq!(facts["application"]["task"], task.content);
    }
    system.shutdown().await;
}

#[tokio::test]
async fn images_from_any_app_expose_reusable_references_to_the_model() {
    use base64::Engine;
    let provider = Arc::new(ReplayProvider::new(vec![
        MockReply::Calls(vec![AgentToolCall {
            id: "inspect-image".into(),
            name: "files__view_image".into(),
            arguments: json!({"path":"image.png","region":null,"resolution":null}),
        }]),
        MockReply::Answer("Image inspected"),
    ]));
    let (_directory, system) = runtime(provider.clone()).await;
    let mut config = system.config();
    config.models.get_mut("mock").unwrap().supports_vision = Some(true);
    system.update_config(config).await.unwrap();
    let png = base64::engine::general_purpose::STANDARD
        .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=")
        .unwrap();
    std::fs::write(
        system
            .actor("root")
            .unwrap()
            .config
            .workspace
            .join("image.png"),
        &png,
    )
    .unwrap();
    let input = system
        .test_actor_input("root", "Inspect the image".into(), vec![], None)
        .unwrap();
    assert_eq!(
        wait_completed(&system, &input.id).await.status,
        MessageStatus::Completed
    );
    let tools = system
        .store()
        .tool_summaries("root", &input.id, None, 10)
        .unwrap();
    let observed = system.store().tool_result(&tools.items[0].id).unwrap();
    assert!(
        observed.error.is_none(),
        "image inspection failed: {:?}",
        observed.error
    );
    let reference = &observed.payload["resource_references"][0];
    let uri = reference["uri"].as_str().unwrap();
    let resource = crate::resources::resolve_reference(&system, "root", uri).unwrap();
    assert_eq!(
        crate::resources::bytes(&system, "root", &resource).unwrap(),
        png
    );
    {
        let requests = provider.requests.lock();
        assert!(
            requests[1].messages.iter().any(
                |row| matches!(row, AgentMessage::Tool { content, .. } if content.contains(uri))
            )
        );
    }
    system.shutdown().await;
}

#[tokio::test]
async fn app_installation_refreshes_tools_between_protocol_groups_without_replaying_input() {
    let server = crate::mcp::tests::server().await;
    let provider = Arc::new(ReplayProvider::new(vec![
        MockReply::Calls(vec![AgentToolCall {
            id: "install-app".into(),
            name: "apps_manager__install".into(),
            arguments: json!({"name":"Example","description":null,"transport":"http","url":server.url,"command":null,"arguments":[],"environment":[],"timeouts":null}),
        }]),
        MockReply::Answer("Ready"),
    ]));
    let (_dir, system) = runtime(provider.clone()).await;
    let input = system
        .test_actor_input("root", "Install the requested App".into(), vec![], None)
        .unwrap();
    let done = wait_completed(&system, &input.id).await;
    assert_eq!(done.status, MessageStatus::Completed);
    assert_eq!(done.reply.as_deref(), Some("Ready"));
    let installed = system
        .store()
        .tool_summaries("root", &input.id, None, 10)
        .unwrap();
    let install = system.store().tool_result(&installed.items[0].id).unwrap();
    assert!(
        install.error.is_none(),
        "installation failed: {:?}",
        install.error
    );
    assert_eq!(
        system.app_registry().entries().len(),
        1,
        "installation outcome: {}",
        install.payload
    );
    {
        let requests = provider.requests.lock();
        assert!(
            !requests[0]
                .tools
                .iter()
                .any(|tool| tool.name.starts_with("mcp_"))
        );
        assert!(
            requests[1]
                .tools
                .iter()
                .any(|tool| tool.name.starts_with("mcp_"))
        );
        assert_eq!(
            system
                .store()
                .messages("root", None, 10)
                .unwrap()
                .items
                .len(),
            1
        );
    }

    system.shutdown().await;
}

#[tokio::test]
async fn apps_manager_queries_and_refresh_preserve_installed_apps() {
    let provider = Arc::new(ReplayProvider::new(vec![
        MockReply::Calls(vec![
            AgentToolCall {
                id: "list-apps".into(),
                name: "apps_manager__list".into(),
                arguments: json!({}),
            },
            AgentToolCall {
                id: "refresh-apps".into(),
                name: "apps_manager__refresh".into(),
                arguments: json!({}),
            },
        ]),
        MockReply::Answer("Done"),
    ]));
    let (_dir, system) = runtime(provider).await;

    let input = system
        .test_actor_input("root", "Inspect available Apps".into(), vec![], None)
        .unwrap();
    assert_eq!(
        wait_completed(&system, &input.id).await.status,
        MessageStatus::Completed
    );
    assert!(
        system
            .store()
            .approvals(Some("root"), None, 10)
            .unwrap()
            .items
            .is_empty()
    );
    assert_eq!(
        system
            .store()
            .tool_summaries("root", &input.id, None, 10)
            .unwrap()
            .items
            .len(),
        2
    );
    system.shutdown().await;
}

#[cfg(any(windows, target_os = "macos", target_os = "android"))]
#[tokio::test]
async fn invalid_browser_arguments_do_not_abort_the_tool_group_or_input() {
    let tab = format!("\"{}\"", "a".repeat(32));
    let provider = Arc::new(ReplayProvider::new(vec![
        MockReply::Calls(vec![
            AgentToolCall {
                id: "invalid-browser-id".into(),
                name: "browser_use__click".into(),
                arguments: json!({"tab_id":tab,"state_id":"b".repeat(32),"x":20,"y":30,"button":null,"click_count":null}),
            },
            AgentToolCall {
                id: "invalid-observation-id".into(),
                name: "browser_use__click".into(),
                arguments: json!({"tab_id":"a".repeat(32),"state_id":format!("\"{}\"", "b".repeat(32)),"x":20,"y":30,"button":null,"click_count":null}),
            },
            AgentToolCall {
                id: "independent-file".into(),
                name: "files__write_file".into(),
                arguments: json!({"path":"continued.txt","content":"continued after invalid input"}),
            },
        ]),
        MockReply::Answer("The invalid browser input was rejected; I can correct it."),
    ]));
    let (_dir, system) = runtime(provider.clone()).await;
    let input = system
        .test_actor_input(
            "root",
            "Test the browser and record progress".into(),
            vec![],
            None,
        )
        .unwrap();
    let completed = wait_completed(&system, &input.id).await;
    assert_eq!(
        completed.status,
        MessageStatus::Completed,
        "{:?}",
        completed.error
    );
    assert_eq!(
        std::fs::read_to_string(
            system
                .actor("root")
                .unwrap()
                .config
                .workspace
                .join("continued.txt")
        )
        .unwrap(),
        "continued after invalid input"
    );
    let tools = system
        .store()
        .tool_summaries("root", &input.id, None, 10)
        .unwrap()
        .items;
    let invalid = system
        .store()
        .tool_result(
            &tools
                .iter()
                .find(|tool| tool.call_id == "invalid-browser-id")
                .unwrap()
                .id,
        )
        .unwrap();
    assert!(invalid.error.is_some());
    let observation_error = tools
        .iter()
        .find(|tool| tool.call_id == "invalid-observation-id")
        .unwrap();
    assert!(observation_error.error.is_some());
    let summary = serde_json::to_value(observation_error).unwrap();
    assert!(summary.get("started_at_ms").is_none());
    assert!(summary.get("finished_at_ms").is_none());
    assert!(
        system
            .store()
            .tool_result(&observation_error.id)
            .unwrap()
            .started_at_ms
            .is_some()
    );
    assert!(
        invalid.started_at_ms.is_some(),
        "Domain target validation runs inside the original invocation"
    );
    assert!(
        tools
            .iter()
            .find(|tool| tool.call_id == "independent-file")
            .unwrap()
            .error
            .is_none()
    );
    assert!(
        system
            .store()
            .approvals(Some("root"), None, 10)
            .unwrap()
            .items
            .is_empty()
    );
    {
        let requests = provider.requests.lock();
        assert_eq!(requests.len(), 2);
        assert!(requests[1].messages.iter().any(|message| matches!(message, AgentMessage::Tool {tool_call_id, content, ..} if tool_call_id == "invalid-browser-id" && content.contains("tab_id"))));
    }
    system.shutdown().await;
}

async fn wait_idle(system: &ActorSystem) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while system
            .actor("root")
            .is_ok_and(|actor| actor.status != ActorStatus::Idle)
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("Actor must settle at the idle boundary");
}

#[tokio::test]
async fn idle_actors_activate_on_demand_and_reload_durable_history() {
    let provider = Arc::new(ReplayProvider::new(vec![
        MockReply::Answer("First"),
        MockReply::Answer("Second"),
    ]));
    let (_dir, system) = runtime(provider.clone()).await;
    system.set_idle_timeout(Duration::from_millis(20));
    let mut root_config = system.actor("root").unwrap();
    root_config.config.apps = vec!["files".into()];
    system.store().save_actor(&root_config).unwrap();
    let root = system.actor("root").unwrap();
    for index in 0..64 {
        let mut config = root.config.clone();
        config.name = format!("Idle worker {index}");
        system.create_owned("root", config).unwrap();
    }
    assert_eq!(system.resident_workers(), 0);
    assert_eq!(system.store().actor_count().unwrap(), 66);
    let first = system
        .test_actor_input("root", "Remember this request".into(), vec![], None)
        .unwrap();
    assert_eq!(
        wait_completed(&system, &first.id).await.status,
        MessageStatus::Completed
    );
    wait_idle(&system).await;
    let context = system.actor("root").unwrap().context;
    assert_eq!(system.store().actor_count().unwrap(), 66);
    assert_eq!(
        system.store().message(&first.id).unwrap().reply.as_deref(),
        Some("First")
    );
    let second = system
        .test_actor_input("root", "Continue".into(), vec![], None)
        .unwrap();
    assert_eq!(
        wait_completed(&system, &second.id).await.status,
        MessageStatus::Completed
    );
    wait_idle(&system).await;
    {
        let requests = provider.requests.lock();
        assert_eq!(requests.len(), 2);
        assert!(requests[1].messages.iter().any(
            |row| matches!(row, AgentMessage::Assistant { content, .. } if content == "First")
        ));
    }
    assert_eq!(
        system.actor("root").unwrap().context.model_binding,
        context.model_binding
    );
    system.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_wakes_execute_each_input_once_across_idle_retirement() {
    let provider = Arc::new(ReplayProvider::new(vec![MockReply::AnswerOnEveryRequest(
        "Done",
    )]));
    let (_dir, system) = runtime(provider.clone()).await;
    system.set_idle_timeout(Duration::from_millis(1));
    let mut root_config = system.actor("root").unwrap();
    root_config.config.apps = vec!["files".into()];
    system.store().save_actor(&root_config).unwrap();
    for round in 0..6 {
        let mut sends = tokio::task::JoinSet::new();
        for input in 0..4 {
            let system = system.clone();
            sends.spawn(async move {
                system
                    .test_actor_input("root", format!("{round}/{input}"), vec![], None)
                    .unwrap()
            });
        }
        while let Some(sent) = sends.join_next().await {
            let message = sent.unwrap();
            let completed = wait_completed(&system, &message.id).await;
            assert_eq!(completed.status, MessageStatus::Completed);
            assert!(
                completed
                    .reply
                    .as_deref()
                    .is_none_or(|reply| reply == "Done")
            );
            let history = system
                .store()
                .history_page("root", 0, Some(&message.id), 128)
                .unwrap();
            assert_eq!(
                history
                    .iter()
                    .filter(|(_, _, row)| matches!(row, AgentMessage::User { content }
                if content.as_text().ends_with(&message.content)))
                    .count(),
                1
            );
        }
        wait_idle(&system).await;
    }
    let messages = system.store().messages("root", None, 100).unwrap().items;
    assert_eq!(messages.len(), 24);
    assert!(
        messages
            .iter()
            .all(|message| message.status == MessageStatus::Completed)
    );
}

#[tokio::test]
async fn reopening_starts_only_queued_actors_and_never_replays_completed_input() {
    let provider = Arc::new(ReplayProvider::new(vec![
        MockReply::Answer("Done"),
        MockReply::Answer("After restart"),
    ]));
    let (dir, system) = runtime(provider.clone()).await;
    system.set_idle_timeout(Duration::from_millis(20));
    let mut root_config = system.actor("root").unwrap();
    root_config.config.apps = vec!["files".into()];
    system.store().save_actor(&root_config).unwrap();
    let root = system.actor("root").unwrap();
    let mut config = root.config.clone();
    config.name = "Queued worker".into();
    let child = system.create_owned("root", config).unwrap();
    let first = system
        .test_actor_input("root", "First".into(), vec![], None)
        .unwrap();
    wait_completed(&system, &first.id).await;
    wait_idle(&system).await;
    system.suspend_execution().await.unwrap();
    let queued = system
        .test_actor_input(&child.id, "Queued".into(), vec![], None)
        .unwrap();
    assert_eq!(system.actor("root").unwrap().status, ActorStatus::Idle);
    system.shutdown().await;
    drop(system);
    let reopened = ActorSystem::open_with_factory(
        dir.path().join("data"),
        root.config.workspace,
        runtime_config(),
        Arc::new(ReplayFactory(provider.clone())),
    )
    .await
    .unwrap();
    assert_eq!(reopened.resident_workers(), 1);
    assert_eq!(
        wait_completed(&reopened, &queued.id).await.reply.as_deref(),
        Some("After restart")
    );
    assert_eq!(
        reopened
            .store()
            .message(&first.id)
            .unwrap()
            .reply
            .as_deref(),
        Some("Done")
    );
    assert_eq!(provider.requests.lock().len(), 2);
    reopened.shutdown().await;
}

#[tokio::test]
async fn partial_answers_and_reasoning_continue_until_a_final_response() {
    let provider = Arc::new(ReplayProvider::new(vec![
        MockReply::FollowUp("Partial answer"),
        MockReply::ReasoningOnly,
        MockReply::Answer("Final answer"),
    ]));
    let (_dir, system) = runtime(provider.clone()).await;
    let input = system
        .test_actor_input("root", "Finish the request".into(), vec![], None)
        .unwrap();
    let completed = wait_completed(&system, &input.id).await;
    assert_eq!(completed.reply.as_deref(), Some("Final answer"));
    {
        let requests = provider.requests.lock();
        assert_eq!(requests.len(), 3);
        assert!(requests[1].messages.iter().any(|row| matches!(row, AgentMessage::Assistant { content, responses_output: Some(output) } if content == "Partial answer" && output[0]["phase"] == "partial_answer")));
        assert!(requests[2].messages.iter().any(|row| {
            row.responses_output()
                .is_some_and(|output| output[0]["encrypted_content"] == "reasoning continuation")
        }));
    }
    system.shutdown().await;
}

#[tokio::test]
async fn empty_model_responses_retry_inference_without_replaying_completed_tool_effects() {
    let provider = Arc::new(ReplayProvider::new(vec![
        MockReply::Calls(vec![AgentToolCall {
            id: "one-effect".into(),
            name: "files__write_file".into(),
            arguments: json!({"path":"once.txt","content":"ONE_EFFECT"}),
        }]),
        MockReply::MalformedEmpty,
        MockReply::Answer("Completed after retry"),
    ]));
    let (_directory, system) = runtime(provider.clone()).await;
    let request = system
        .test_actor_input("root", "Complete the task".into(), vec![], None)
        .unwrap();
    let done = wait_completed(&system, &request.id).await;
    assert_eq!(done.status, MessageStatus::Completed, "{:?}", done.error);
    assert_eq!(done.reply.as_deref(), Some("Completed after retry"));
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
        "ONE_EFFECT"
    );
    assert_eq!(
        system
            .store()
            .tool_summaries("root", &request.id, None, 10)
            .unwrap()
            .items
            .len(),
        1
    );
    let requests = provider.requests.lock().clone();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        serde_json::to_value(&requests[1]).unwrap(),
        serde_json::to_value(&requests[2]).unwrap()
    );
    let dump = std::fs::read_to_string(
        system
            .actor_state_dir("root")
            .join("diagnostics/last_response.txt"),
    )
    .unwrap();
    assert!(dump.contains("status=ok") && dump.contains("attempt=2"));
    system.shutdown().await;
}

#[tokio::test]
async fn empty_model_response_retry_exhaustion_is_a_model_failure_with_private_diagnostics() {
    let provider = Arc::new(ReplayProvider::new(
        (0..6).map(|_| MockReply::MalformedEmpty).collect(),
    ));
    let (_directory, system) = runtime(provider.clone()).await;
    let request = system
        .test_actor_input("root", "Complete the task".into(), vec![], None)
        .unwrap();
    let done = wait_completed(&system, &request.id).await;
    assert_eq!(done.status, MessageStatus::Failed);
    assert_eq!(done.error.as_deref(), Some("model_request_failed"));
    assert_eq!(provider.requests.lock().len(), 6);
    let dump = std::fs::read_to_string(
        system
            .actor_state_dir("root")
            .join("diagnostics/last_response.txt"),
    )
    .unwrap();
    assert!(
        dump.contains("status=error")
            && dump.contains("attempt=6")
            && dump.contains("will_retry=false")
    );
    assert!(dump.contains("no assistant text"));
    system.shutdown().await;
}

#[tokio::test]
async fn empty_reasoning_has_a_fuse_and_transient_requests_can_reach_the_fifth_attempt() {
    let provider = Arc::new(ReplayProvider::new(vec![
        MockReply::ReasoningOnly,
        MockReply::ReasoningOnly,
        MockReply::ReasoningOnly,
    ]));
    let (_dir, system) = runtime(provider.clone()).await;
    let input = system
        .test_actor_input("root", "Finish".into(), vec![], None)
        .unwrap();
    assert_eq!(
        wait_completed(&system, &input.id).await.status,
        MessageStatus::Failed
    );
    assert_eq!(provider.requests.lock().len(), 3);
    system.shutdown().await;
    let provider = Arc::new(ReplayProvider::new(vec![
        MockReply::Failure("connection reset"),
        MockReply::Failure("connection reset"),
        MockReply::Failure("connection reset"),
        MockReply::Failure("connection reset"),
        MockReply::Answer("Recovered"),
    ]));
    let (_dir, system) = runtime(provider.clone()).await;
    let input = system
        .test_actor_input("root", "Retry".into(), vec![], None)
        .unwrap();
    assert_eq!(
        wait_completed(&system, &input.id).await.reply.as_deref(),
        Some("Recovered")
    );
    assert_eq!(provider.requests.lock().len(), 5);
    let dump = std::fs::read_to_string(
        system
            .actor_state_dir("root")
            .join("diagnostics/last_response.txt"),
    )
    .unwrap();
    assert!(dump.contains("status=ok") && dump.contains("attempt=5"));
    system.shutdown().await;
}

#[tokio::test]
async fn queued_inputs_and_actor_replies_survive_beyond_the_former_mailbox_quota() {
    let (_directory, system, _provider) = paused_runtime().await;
    let original = system
        .test_actor_input("root", "Coordinate work".into(), vec![], None)
        .unwrap();
    system.store().claim_next("root").unwrap().unwrap();
    let child = system
        .create_owned("root", project_config(&system, "reporter"))
        .unwrap();
    let request = system
        .message_from_actor("root", &original.id, &child.id, "Investigate".into())
        .unwrap();
    system.store().claim_next(&child.id).unwrap().unwrap();
    for index in 0..300 {
        system
            .test_actor_input("root", format!("Pending input {index}"), vec![], None)
            .unwrap();
    }
    let progress = system
        .deliver_from_actor(&child.id, &request.id, "progress", "Independent result")
        .unwrap();
    assert!(
        system
            .store()
            .message(&progress.id)
            .unwrap()
            .content
            .ends_with("Independent result")
    );
    let count: i64 = system
        .store()
        .stage_transaction(|tx| {
            Ok(tx.query_row(
                "SELECT COUNT(*) FROM messages WHERE actor_id='root' AND status='\"queued\"'",
                [],
                |row| row.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(count, 301);
    system.shutdown().await;
}

#[tokio::test]
async fn actor_listing_spills_to_temporary_storage_without_failing_a_nearly_full_legacy_resource_ledger()
 {
    let provider = Arc::new(ReplayProvider::new(vec![
        MockReply::Calls(vec![AgentToolCall {
            id: "roster".into(),
            name: "actors__list".into(),
            arguments: json!({"stage_id":null,"cursor":null,"limit":100}),
        }]),
        MockReply::Answer("Roster inspected"),
    ]));
    let (_directory, system) = runtime(provider.clone()).await;
    let mut config = system.config();
    config
        .models
        .get_mut("mock")
        .unwrap()
        .tool_output_max_tokens = 512;
    system.update_config(config).await.unwrap();
    for index in 0..32 {
        let id = format!("retained-{index}");
        system.seed_test_actor(&id);
        let mut actor = system.actor(&id).unwrap();
        actor.config.name = format!(
            "Collaborator {index} {}",
            "long task description ".repeat(12)
        );
        system.store().save_actor(&actor).unwrap();
    }
    let path = system.actor_data_dir("root").join("retained-large.bin");
    let size = 512 * 1024 * 1024 - 7553;
    std::fs::File::create(&path).unwrap().set_len(size).unwrap();
    system
        .store()
        .register_resource(super::super::StoredResource {
            expires_at_ms: None,
            id: "retained-large".into(),
            actor_id: "root".into(),
            path,
            name: "retained-large".into(),
            media_type: "application/octet-stream".into(),
            size_bytes: size,
            uri: "/v1/actors/root/resources/retained-large".into(),
        })
        .unwrap();
    let request = system
        .test_actor_input("root", "Inspect the roster".into(), vec![], None)
        .unwrap();
    let done = wait_completed(&system, &request.id).await;
    assert_eq!(done.status, MessageStatus::Completed, "{:?}", done.error);
    assert_eq!(done.reply.as_deref(), Some("Roster inspected"));
    let calls = system
        .store()
        .tool_summaries("root", &request.id, None, 8)
        .unwrap()
        .items;
    assert_eq!(calls.len(), 1);
    assert!(calls[0].error.is_none());
    let result = system.store().tool_result(&calls[0].id).unwrap();
    let id:String=system.store().stage_transaction(|tx|Ok(tx.query_row("SELECT id FROM resources WHERE actor_id='root' AND json_extract(record,'$.expires_at_ms') IS NOT NULL",[],|row|row.get(0))?)).unwrap();
    let resource = crate::resources::export(&system, "root", &id).unwrap();
    let complete = crate::resources::bytes(&system, "root", &resource).unwrap();
    assert!(complete.len() > 7553);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&complete).unwrap(),
        result.payload
    );
    assert!(result.model_content.contains(&resource.uri("root")));
    assert_eq!(provider.requests.lock().len(), 2);
    system.shutdown().await;
}

#[tokio::test]
async fn complete_output_remains_readable_after_truncation_and_usage_and_diagnostics_survive() {
    let provider = Arc::new(ReplayProvider::new(vec![
        MockReply::Answer("Done"),
        MockReply::Answer("Next"),
    ]));
    provider.usage_bias.store(25, Ordering::Relaxed);
    let (dir, system) = runtime(provider.clone()).await;
    let original = "完整输出\n".repeat(2000) + "TAIL-MARKER";
    let bounded =
        crate::resources::bound_output(&system, "root", "large-call", &original, 256, None)
            .unwrap();
    assert!(bounded.len() <= 1024);
    let resource = crate::resources::export_all(&system, "root")
        .unwrap()
        .into_iter()
        .find(|resource| resource.name.starts_with("output-"))
        .unwrap();
    assert!(bounded.contains(&resource.id));
    assert!(bounded.contains(&resource.uri("root")));
    let input = system
        .test_actor_input(
            "root",
            format!(
                "Inspect {} and redact unused-test-key",
                "long input ".repeat(500)
            ),
            vec![],
            None,
        )
        .unwrap();
    assert_eq!(
        wait_completed(&system, &input.id).await.status,
        MessageStatus::Completed
    );
    let first_usage = system.actor("root").unwrap().context.usage;
    let first_models = system.actor("root").unwrap().context.model_usage;
    assert_eq!(first_models.len(), 1);
    assert_eq!(first_models[0].model, "mock");
    assert_eq!(first_models[0].provider, "mock");
    assert_eq!(first_models[0].model_id, "mock");
    assert_eq!(first_models[0].usage, first_usage);
    assert!(
        first_usage.total_token_usage.input_tokens > 0 && first_usage.daily_token_usage.len() == 1
    );
    let diagnostic = std::fs::read_to_string(
        system
            .actor_state_dir("root")
            .join("diagnostics/messages.txt"),
    )
    .unwrap();
    assert!(
        diagnostic.len() > 2000
            && !diagnostic.contains("unused-test-key")
            && diagnostic.contains("[redacted]")
    );
    let workspace = system.actor("root").unwrap().config.workspace;
    system.shutdown().await;
    drop(system);
    let system = ActorSystem::open_with_factory(
        dir.path().join("data"),
        workspace,
        runtime_config(),
        Arc::new(ReplayFactory(provider)),
    )
    .await
    .unwrap();
    assert_eq!(system.actor("root").unwrap().context.usage, first_usage);
    assert_eq!(
        system.actor("root").unwrap().context.model_usage,
        first_models
    );
    let mut config = runtime_config();
    let mut replacement = config.models.remove("mock").unwrap();
    replacement.model_id = "replacement-v2".into();
    config.models.insert("replacement".into(), replacement);
    config.main_model = "replacement".into();
    system.update_config(config).await.unwrap();
    assert_eq!(
        crate::resources::bytes(&system, "root", &resource).unwrap(),
        original.as_bytes()
    );
    let input = system
        .test_actor_input("root", "Next".into(), vec![], None)
        .unwrap();
    assert_eq!(
        wait_completed(&system, &input.id).await.status,
        MessageStatus::Completed
    );
    assert!(
        system
            .actor("root")
            .unwrap()
            .context
            .usage
            .total_token_usage
            .input_tokens
            > first_usage.total_token_usage.input_tokens
    );
    let updated = system.actor("root").unwrap().context;
    assert_eq!(updated.model_usage.len(), 2);
    assert_eq!(updated.model_usage[0], first_models[0]);
    assert_eq!(updated.model_usage[1].model, "replacement");
    assert_eq!(updated.model_usage[1].model_id, "replacement-v2");
    let mut sum = first_models[0].usage.total_token_usage.clone();
    sum.add_assign(&updated.model_usage[1].usage.total_token_usage);
    assert_eq!(sum, updated.usage.total_token_usage);
    system.shutdown().await;
}

struct ReplayProvider {
    replies: parking_lot::Mutex<VecDeque<MockReply>>,
    requests: parking_lot::Mutex<Vec<AgentTurnRequest>>,
    started: tokio::sync::Notify,
    dropped: Arc<AtomicBool>,
    pending: Arc<std::sync::atomic::AtomicUsize>,
    usage: parking_lot::Mutex<TokenUsageInfo>,
    usage_bias: std::sync::atomic::AtomicUsize,
    auto_compact_threshold: std::sync::atomic::AtomicUsize,
    review_requests: parking_lot::Mutex<Vec<PromptRequest>>,
    review_replies: parking_lot::Mutex<VecDeque<serde_json::Value>>,
    review_blocked: AtomicBool,
    review_release: tokio::sync::Notify,
    inference_gate: parking_lot::Mutex<Option<Arc<tokio::sync::Notify>>>,
}

impl ReplayProvider {
    fn new(replies: Vec<MockReply>) -> Self {
        Self {
            replies: parking_lot::Mutex::new(replies.into()),
            requests: parking_lot::Mutex::new(Vec::new()),
            started: tokio::sync::Notify::new(),
            dropped: Arc::new(AtomicBool::new(false)),
            pending: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            usage: parking_lot::Mutex::new(TokenUsageInfo::default()),
            usage_bias: std::sync::atomic::AtomicUsize::new(0),
            auto_compact_threshold: std::sync::atomic::AtomicUsize::new(100_000),
            review_requests: parking_lot::Mutex::new(Vec::new()),
            review_replies: parking_lot::Mutex::new(VecDeque::new()),
            review_blocked: AtomicBool::new(false),
            review_release: tokio::sync::Notify::new(),
            inference_gate: parking_lot::Mutex::new(None),
        }
    }
}

struct DropMarker(Arc<AtomicBool>, Arc<std::sync::atomic::AtomicUsize>);
impl Drop for DropMarker {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
        self.1.fetch_sub(1, Ordering::AcqRel);
    }
}

#[async_trait]
impl ModelProvider for ReplayProvider {
    async fn complete_json(
        &self,
        request: PromptRequest,
        _: ModelRequestOptions,
    ) -> Result<serde_json::Value> {
        self.review_requests.lock().push(request);
        if self.review_blocked.load(Ordering::Acquire) {
            self.review_release.notified().await;
        }
        Ok(self
            .review_replies
            .lock()
            .pop_front()
            .expect("Unexpected independent review model request"))
    }
    async fn complete_agent_turn(
        &self,
        request: AgentTurnRequest,
        options: ModelRequestOptions,
    ) -> Result<AgentTurnStreamResult> {
        let raw_input = estimate_agent_turn_request(
            &request.messages,
            &request.tools,
            self.request_budget_limits(),
        )
        .total_input_tokens;
        self.requests.lock().push(request.clone());
        self.started.notify_one();
        let reply = {
            let mut replies = self.replies.lock();
            match replies.front() {
                Some(MockReply::PendingOnEveryRequest) => MockReply::PendingOnEveryRequest,
                Some(MockReply::AnswerOnEveryRequest(text)) => MockReply::Answer(text),
                _ => replies.pop_front().expect("mock reply available"),
            }
        };
        let gate = self.inference_gate.lock().take();
        if let Some(gate) = gate {
            gate.notified().await;
        }
        let reply = match reply {
            MockReply::Gated(reply, gate) => {
                gate.notified().await;
                *reply
            }
            reply => reply,
        };
        let bias = self.usage_bias.load(Ordering::Relaxed);
        if bias > 0 && !matches!(reply, MockReply::Pending | MockReply::PendingOnEveryRequest) {
            self.usage
                .lock()
                .append_last_usage(crate::core::TokenUsage {
                    input_tokens: (raw_input + bias) as i64,
                    ..Default::default()
                });
        }
        if let Some(progress) = options.progress {
            for _ in 0..50 {
                progress.emit_assistant_delta("small delta ");
            }
        }
        match reply {
            MockReply::Gated(..) => unreachable!("nested test gates"),
            MockReply::Failure(error) => Err(miette!("{error}")),
            MockReply::MalformedEmpty => Ok(AgentTurnStreamResult {
                items: vec![],
                raw_stream_follow_up: false,
                end_turn: false,
                last_assistant_message: None,
                last_reasoning_content: None,
                last_reasoning_signature: None,
                responses_output: None,
            }),
            MockReply::ReasoningOnly => Ok(AgentTurnStreamResult {
                items: vec![],
                raw_stream_follow_up: true,
                end_turn: false,
                last_assistant_message: None,
                last_reasoning_content: Some("continue thinking".into()),
                last_reasoning_signature: None,
                responses_output: Some(vec![
                    json!({"type":"reasoning","encrypted_content":"reasoning continuation"}),
                ]),
            }),
            MockReply::FollowUp(text) => Ok(AgentTurnStreamResult {
                items: vec![],
                raw_stream_follow_up: true,
                end_turn: false,
                last_assistant_message: Some(text.into()),
                last_reasoning_content: None,
                last_reasoning_signature: None,
                responses_output: Some(vec![
                    json!({"type":"message","phase":"partial_answer","content":[{"type":"output_text","text":text}]}),
                ]),
            }),
            MockReply::Answer(text) | MockReply::AnswerOnEveryRequest(text) => {
                Ok(AgentTurnStreamResult {
                    items: vec![],
                    raw_stream_follow_up: false,
                    end_turn: true,
                    last_assistant_message: Some(text.into()),
                    last_reasoning_content: None,
                    last_reasoning_signature: None,
                    responses_output: Some(vec![
                        json!({"type":"message","content":[{"type":"output_text","text":text}]}),
                    ]),
                })
            }
            MockReply::Calls(calls) => {
                let mut output =
                    vec![json!({"type":"reasoning","encrypted_content":"encrypted reasoning"})];
                output.extend(calls.iter().map(|call| {
                    json!({"type":"function_call", "id":call.id,
                    "call_id":call.id,"name":call.name,"arguments":call.arguments.to_string()})
                }));
                Ok(AgentTurnStreamResult {
                    items: calls
                        .into_iter()
                        .map(|call| crate::model_protocol::AgentTurnItem::ToolCall { call })
                        .collect(),
                    raw_stream_follow_up: true,
                    end_turn: false,
                    last_assistant_message: Some("Inspecting".into()),
                    last_reasoning_content: Some("private reasoning".into()),
                    last_reasoning_signature: Some("provider signature".into()),
                    responses_output: Some(output),
                })
            }
            MockReply::Pending | MockReply::PendingOnEveryRequest => {
                self.pending.fetch_add(1, Ordering::AcqRel);
                let _marker = DropMarker(self.dropped.clone(), self.pending.clone());
                std::future::pending().await
            }
        }
    }
    fn request_budget_limits(&self) -> RequestBudgetLimits {
        RequestBudgetLimits {
            context_window_tokens: 100_000,
            auto_compact_threshold_tokens: self.auto_compact_threshold.load(Ordering::Relaxed),
            reserved_output_tokens: 2_000,
        }
    }
    fn token_usage_info(&self) -> TokenUsageInfo {
        self.usage.lock().clone()
    }
    fn model_name(&self) -> String {
        "mock-runtime".into()
    }
}

struct ReplayFactory(Arc<ReplayProvider>);

impl ModelFactory for ReplayFactory {
    fn build(&self, _: &str, _: &crate::config::Config) -> Result<Arc<dyn ModelProvider>> {
        Ok(self.0.clone())
    }
}

struct ModelSwitchFactory {
    core: Arc<ReplayProvider>,
    worker: Arc<ReplayProvider>,
}
impl ModelFactory for ModelSwitchFactory {
    fn build(&self, name: &str, _: &crate::config::Config) -> Result<Arc<dyn ModelProvider>> {
        Ok(if name == "worker" {
            self.worker.clone()
        } else {
            self.core.clone()
        })
    }
}

async fn device_pair(
    model_a: Arc<ReplayProvider>,
    model_b: Arc<ReplayProvider>,
) -> (
    (
        tempfile::TempDir,
        ActorSystem,
        Arc<crate::stage::runtime::Runtime>,
    ),
    (
        tempfile::TempDir,
        ActorSystem,
        Arc<crate::stage::runtime::Runtime>,
    ),
    crate::stage::test_relay::Relay,
) {
    let relay = crate::stage::test_relay::Relay::new().await;
    use crate::stage::{host::Host, ledger::StageLedger, runtime::Runtime};
    let (a_dir, a) = runtime(model_a).await;
    let (b_dir, b) = runtime(model_b).await;
    let host_a = Host::load(a.clone()).unwrap();
    host_a.initialize(true).unwrap();
    let host_b = Host::load(b.clone()).unwrap();
    let request = host_b.initialize(false).unwrap();
    host_b
        .join(
            fixture_enrollment(&host_a, &request, true, Default::default()).unwrap(),
            &host_a.account_id().unwrap(),
        )
        .unwrap();
    StageLedger::attach(b.store(), host_b.local)
        .import_peer(
            host_a.enrollment().unwrap(),
            Default::default(),
            chrono::Utc::now().timestamp_millis(),
        )
        .unwrap();
    for host in [&host_a, &host_b] {
        let mut settings = host.settings();
        settings.mode = crate::stage::host::Connectivity::Relay;
        settings.relay_origin = Some(relay.endpoint.clone());
        host.save_settings(settings).unwrap();
    }
    let net_a = Runtime::new(host_a);
    let net_b = Runtime::new(host_b);
    net_a.start().await.unwrap();
    net_b.start().await.unwrap();
    crate::stage::test_relay::wait(|| net_b.handle(net_a.host.local).is_ok())
        .await
        .unwrap();
    ((a_dir, a, net_a), (b_dir, b, net_b), relay)
}

fn assert_shared_actor_instructions(prompt: &str) {
    let instructions = include_str!("../prompts/actors.md")
        .split("{{models}}")
        .next()
        .unwrap();
    assert_eq!(prompt.matches(instructions).count(), 1);
    assert!(!prompt.contains(include_str!("../prompts/approval_review.md")));
    assert!(!prompt.contains("Use report for a material discovery"));
    assert!(!prompt.contains("Read this Actor's own durable history"));
}

fn runtime_config() -> crate::config::Config {
    let mut config = crate::config::Config {
        main_model: "mock".into(),
        ..Default::default()
    };
    config.models.insert(
        "mock".into(),
        crate::config::ModelConfig {
            provider: "mock".into(),
            model_id: "mock".into(),
            supports_vision: Some(true),
            ..Default::default()
        },
    );
    config.providers.insert(
        "mock".into(),
        crate::config::ProviderConfig::OpenaiCompatible {
            base_url: "https://example.invalid".into(),
            api_key: "unused-test-key".into(),
        },
    );
    config
}

async fn runtime(provider: Arc<ReplayProvider>) -> (tempfile::TempDir, ActorSystem) {
    let directory = crate::sandbox::test_support::scoped_directory();
    let workspace = directory.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let system = ActorSystem::open_with_factory(
        directory.path().join("data"),
        workspace,
        runtime_config(),
        Arc::new(ReplayFactory(provider)),
    )
    .await
    .unwrap();
    system.seed_test_actor("root");

    (directory, system)
}

#[tokio::test]
async fn remote_delegation_retains_readonly_authority() {
    use crate::stage::{
        identity::DeviceIdentity,
        ledger::{DeviceAuthority, StageLedger},
    };
    let (_directory, system, _) = paused_runtime().await;
    let child = system
        .create_owned("root", project_config(&system, "delegated worker"))
        .unwrap();
    let mut host =
        DeviceIdentity::generate_for_stage(system.store().public_stage_id().unwrap()).unwrap();
    let remote = DeviceIdentity::generate().unwrap();
    let now = chrono::Utc::now().timestamp_millis();
    let root = host.initialize_group(now, now + 600000).unwrap();
    let enrollment = host
        .authorize(
            &root,
            &remote.join_request(now).unwrap(),
            now,
            now + 300000,
            false,
        )
        .unwrap();
    let ledger =
        StageLedger::initialize(system.store(), host.profile().stage_id, &root.root).unwrap();
    system
        .store()
        .stage_transaction(|tx| {
            tx.execute_batch("CREATE TABLE IF NOT EXISTS stage_account_actors(actor TEXT PRIMARY KEY REFERENCES actors(id) ON DELETE CASCADE,account TEXT NOT NULL);")?;
            tx.execute(
                "INSERT OR REPLACE INTO stage_account_actors VALUES(?1,?2)",
                rusqlite::params!["core", root.root.group_id.to_string()],
            )?;
            tx.execute(
                "INSERT OR REPLACE INTO stage_account_actors VALUES(?1,?2)",
                rusqlite::params![child.id, root.root.group_id.to_string()],
            )?;
            Ok(())
        })
        .unwrap();
    ledger
        .import_peer(
            enrollment,
            DeviceAuthority {
                read: true,
                converse: true,
                delegate: true,
                read_only: true,
            },
            now,
        )
        .unwrap();
    let input = MessageRecord {
        skills: Vec::new(),
        seq: 0,
        id: uuid::Uuid::new_v4().to_string(),
        actor_id: "core".into(),
        source: super::super::MessageSource::RemoteUser {
            stage_id: remote.profile().stage_id,
            invocation_id: uuid::Uuid::new_v4(),
        },
        reply_to: super::super::ReplyTarget::User,
        kind: super::super::MessageKind::Request,
        in_reply_to: None,

        remote_authority: Some(super::super::RemoteAuthority {
            read_only: true,
            peer: remote.profile().stage_id,
            origin_actor: "core".into(),
            reply_for: None,
        }),
        content: "Delegate a read-only investigation".into(),
        attachments: vec![],
        status: MessageStatus::Queued,
        cancel_requested: false,
        started_at_ms: None,
        finished_at_ms: None,
        final_generation: None,
        reply: None,
        error: None,
        created_at_ms: now,
        updated_at_ms: now,
    };
    let input = system.store().enqueue(input, None).unwrap();
    system.store().claim_next("core").unwrap().unwrap();
    let delegated = system
        .message_from_actor("core", &input.id, &child.id, "Inspect the project".into())
        .unwrap();
    let execution = system.execution_actor(&delegated).unwrap();
    assert!(execution.remote_read_only);
    system.store().claim_next(&child.id).unwrap().unwrap();
    let context = system.execution_context(&execution, &delegated).unwrap();
    assert!(
        system
            .requested_tool_context(
                &context,
                &crate::sandbox::SandboxScopeRequest {
                    writable_roots: vec![child.config.workspace.clone()],
                    ..Default::default()
                }
            )
            .is_err()
    );
    // Revoking the original device grant also invalidates already queued descendants.
    ledger
        .import_peer(
            host.authorize(
                &root,
                &remote.join_request(now).unwrap(),
                now,
                now + 300000,
                false,
            )
            .unwrap(),
            DeviceAuthority {
                read: true,
                converse: true,
                delegate: false,
                read_only: true,
            },
            now,
        )
        .unwrap();
    assert!(system.execution_actor(&delegated).is_err());
    system.shutdown().await;
}

async fn wait_completed(system: &ActorSystem, id: &str) -> MessageRecord {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let message = system.store().message(id).unwrap();
        if !matches!(
            message.status,
            MessageStatus::Queued | MessageStatus::Running
        ) {
            return message;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "Actor request did not finish: {message:?}; actor={:?}",
            system.actor(&message.actor_id).map(|a| a.status),
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn native_stage_tcp_routes_durable_user_work_to_a_session_core() {
    let relay = crate::stage::test_relay::Relay::new().await;
    use crate::stage::{
        ActorAddress,
        host::Host,
        ledger::{DeviceAuthority, StageLedger},
        protocol::{Body, SubscriptionLevel},
        runtime::Runtime,
        sync::Replica,
    };
    let model_a = Arc::new(ReplayProvider::new(vec![]));
    let (_a_dir, a) = runtime(model_a).await;
    let model_b = Arc::new(ReplayProvider::new(vec![MockReply::Answer(
        "TCP actor response",
    )]));
    let (_b_dir, b) = runtime(model_b.clone()).await;
    let host_a = Host::load(a.clone()).unwrap();
    let host_b = Host::load(b.clone()).unwrap();
    host_a.initialize(true).unwrap();
    let request_b = host_b.initialize(false).unwrap();
    let authority = DeviceAuthority {
        read: true,
        converse: true,
        delegate: true,
        read_only: false,
    };
    let enrollment_b = fixture_enrollment(&host_a, &request_b, false, authority.clone()).unwrap();
    host_b
        .join(enrollment_b, &host_a.account_id().unwrap())
        .unwrap();
    StageLedger::attach(b.store(), host_b.local)
        .import_peer(
            host_a.enrollment().unwrap(),
            authority,
            chrono::Utc::now().timestamp_millis(),
        )
        .unwrap();
    for host in [&host_a, &host_b] {
        let mut settings = host.settings();

        settings.mode = crate::stage::host::Connectivity::Relay;
        settings.relay_origin = Some(relay.endpoint.clone());
        host.save_settings(settings).unwrap();
    }
    let net_a = Runtime::new(host_a.clone());
    let net_b = Runtime::new(host_b.clone());
    net_a.start().await.unwrap();
    net_b.start().await.unwrap();
    crate::stage::test_relay::wait(|| net_a.handle(host_b.local).is_ok())
        .await
        .unwrap();
    let target = ActorAddress {
        stage_id: host_b.local,
        actor_id: "core".into(),
    };
    let view = uuid::Uuid::new_v4();
    let handle = net_a.handle(host_b.local).unwrap();
    handle
        .watch("core", view, SubscriptionLevel::Detail)
        .unwrap();
    let send = || {
        crate::stage::delivery::Deliveries::new(a.store(), host_a.local).queue(
            &a.actor("core").unwrap(),
            None,
            target.clone(),
            ("Respond over the native peer channel.".into(), vec![]),
            chrono::Utc::now().timestamp_millis(),
            "tcp-client-operation",
        )
    };
    let frame = send().unwrap();
    assert_eq!(send().unwrap().invocation_id, frame.invocation_id);
    handle.flush();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let done = a
            .store()
            .stage_transaction(|tx| {
                let found: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM stage_received_results WHERE invocation=?1)",
                    [frame.invocation_id.unwrap().to_string()],
                    |r| r.get(0),
                )?;
                Ok(found)
            })
            .unwrap();
        if done
            && Replica::new(a.store())
                .progress(&target, chrono::Utc::now().timestamp_millis())
                .is_ok_and(|state| state.caught_up)
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "Native Stage did not finish: A={:?} B={:?}, errors A={:?} B={:?}",
            net_a.state(host_b.local),
            net_b.state(host_a.local),
            net_a.last_error(host_b.local),
            net_b.last_error(host_a.local)
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(model_b.requests.lock().len(), 1);
    let directory = handle
        .rpc(Body::DirectoryQuery {
            after: None,
            limit: 32,
        })
        .await
        .unwrap();
    assert!(
        matches!(directory,Body::Directory{actors,..}if actors.len()==2 && actors.iter().any(|actor| actor.address.actor_id=="root") && actors.iter().any(|actor|actor.address.actor_id=="core"))
    );
    handle.unwatch("core", view);
    net_a.stop().await;
    net_b.stop().await;
    a.shutdown().await;
    b.shutdown().await;
}
#[tokio::test]
async fn remote_delegation_delivers_a_correlated_reply_without_peer_owner_authority() {
    use crate::stage::{
        ActorAddress,
        blob::ContentStore,
        delivery::Deliveries,
        identity::DeviceIdentity,
        ledger::{DeviceAuthority, StageLedger},
        protocol::{BlobAccess, Body},
        receiver::StageReceiver,
        transport::tls,
    };
    let peer_model = Arc::new(ReplayProvider::new(vec![
        MockReply::Calls(vec![AgentToolCall {
            id: "remote-write-once".into(),
            name: "files__write_file".into(),
            arguments: json!({"path":"one.txt","content":"one effect"}),
        }]),
        MockReply::Calls(vec![AgentToolCall {
            id: "remote-progress".into(),
            name: "actors__reply".into(),
            arguments: json!({"content":"The file is written; checking the result."}),
        }]),
        MockReply::Answer(""),
    ]));
    let (_peer_dir, b) = runtime(peer_model.clone()).await;
    let b_id = b.store().public_stage_id().unwrap();
    let caller_model = Arc::new(ReplayProvider::new(vec![
        MockReply::Calls(vec![AgentToolCall {
            id: "dispatch-once".into(),
            name: "actors__message".into(),
            arguments: json!({"reply_to":null,"actor_id":"root","stage_id":b_id.to_string(),"content":"Write one file and report."}),
        }]),
        MockReply::Answer("The task is delegated."),
        MockReply::Answer("The collaborator reported progress."),
    ]));
    let (_caller_dir, a) = runtime(caller_model.clone()).await;
    let a_id = a.store().public_stage_id().unwrap();
    let now = chrono::Utc::now().timestamp_millis();
    let mut identity_a = DeviceIdentity::generate_for_stage(a_id).unwrap();
    let identity_b = DeviceIdentity::generate_for_stage(b_id).unwrap();
    let own = identity_a.initialize_group(now, now + 600000).unwrap();
    let member_b = identity_a
        .authorize(
            &own,
            &identity_b.join_request(now).unwrap(),
            now,
            now + 300000,
            false,
        )
        .unwrap();
    let ledger_a = StageLedger::initialize(a.store(), a_id, &own.root).unwrap();
    let ledger_b = StageLedger::initialize(b.store(), b_id, &own.root).unwrap();
    ledger_a
        .import_peer(member_b, Default::default(), now)
        .unwrap();
    ledger_b
        .import_peer(
            own,
            DeviceAuthority {
                read: true,
                converse: true,
                delegate: true,
                read_only: false,
            },
            now,
        )
        .unwrap();
    let nonce = uuid::Uuid::new_v4();
    ledger_a.commit_generation(b_id, 0, 1, nonce, now).unwrap();
    ledger_b.commit_generation(a_id, 0, 1, nonce, now).unwrap();
    b.store()
        .stage_transaction(|tx| {
            tx.execute(
                "INSERT OR REPLACE INTO actor_session_hosts(actor,host) VALUES('root',?1)",
                [a_id.to_string()],
            )?;
            Ok(())
        })
        .unwrap();
    let acceptor = tls::server(&identity_b, &[identity_a.profile()]).unwrap();
    let connector = tls::client(&identity_a, &identity_b.profile()).unwrap();
    let (left, right) = tokio::io::duplex(65536);
    let (server, client) = tokio::join!(
        tls::accept(&acceptor, left),
        tls::connect(&connector, b_id, right)
    );
    let server = server.unwrap();
    let client = client.unwrap();
    let incoming_a = tls::identify_server(&client, &identity_b.profile()).unwrap();
    let incoming_b = tls::identify_client(&server, &[identity_a.profile()]).unwrap();
    let original = a
        .test_actor_input(
            "root",
            "Delegate one task.".into(),
            vec![],
            Some("remote-parent".into()),
        )
        .unwrap();
    wait_completed(&a, &original.id).await;
    let frame = ledger_a.pending(b_id, now, 10).unwrap().pop().unwrap();
    let invocation = frame.invocation_id.unwrap();
    let Body::Invoke { content, .. } = &frame.body else {
        panic!("Expected invocation");
    };
    let manifest = ledger_a
        .exported_manifest(b_id, invocation, content, now)
        .unwrap();
    let receiver_b = StageReceiver::new(b.clone());
    assert_eq!(
        receiver_b
            .prepare_content(&incoming_b, &frame, &manifest)
            .unwrap(),
        vec![0]
    );
    let bytes = ledger_a
        .exported_chunk(b_id, invocation, content, 0, now)
        .unwrap();
    receiver_b
        .receive_content_chunk(&incoming_b, &frame, 0, &bytes)
        .unwrap();
    let accepted = receiver_b.accept_cached_input(&incoming_b, &frame).unwrap();
    assert!(
        receiver_b
            .accept_cached_input(&incoming_b, &frame)
            .unwrap()
            .duplicate
    );
    let completed_b = wait_completed(&b, accepted.message_id.as_ref().unwrap()).await;
    let b_results = b
        .store()
        .tool_summaries("root", &completed_b.id, None, 100)
        .unwrap();
    assert!(
        b_results.items.iter().all(|result| result.error.is_none()),
        "Remote tool failed: {:?}",
        b_results.items
    );
    assert_eq!(
        std::fs::read_to_string(b.actor("root").unwrap().config.workspace.join("one.txt")).unwrap(),
        "one effect"
    );
    assert_eq!(peer_model.requests.lock().len(), 3);
    let mut replies = Deliveries::new(b.store(), b_id)
        .ready(a_id, chrono::Utc::now().timestamp_millis(), 10)
        .unwrap();
    assert_eq!(replies.len(), 2);
    let index = replies
        .iter()
        .position(|frame| {
            matches!(
                frame.body,
                Body::Delivery {
                    state: crate::stage::protocol::InvocationState::Running,
                    initial: false,
                    ..
                }
            )
        })
        .expect("report is not completion");
    let mut progress = replies.remove(index);
    progress.generation = 1;
    let Body::Delivery { content, .. } = &progress.body else {
        panic!("Expected progress");
    };
    let receiver_a = StageReceiver::new(a.clone());
    let manifest = ContentStore::new(b.store())
        .manifest(a_id, "root", content, now)
        .unwrap();
    receiver_a
        .prepare_content(&incoming_a, &progress, &manifest)
        .unwrap();
    let bytes = ContentStore::new(b.store())
        .chunk(a_id, "root", content, 0, now)
        .unwrap();
    receiver_a
        .receive_content_chunk(&incoming_a, &progress, 0, &bytes)
        .unwrap();
    receiver_a
        .accept_cached_reply(&incoming_a, &progress)
        .unwrap();
    receiver_a
        .accept_cached_reply(&incoming_a, &progress)
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if a.store()
                .messages("root", None, 100)
                .unwrap()
                .items
                .iter()
                .any(|m| m.kind == MessageKind::Progress && m.status == MessageStatus::Completed)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        a.store()
            .messages("root", None, 100)
            .unwrap()
            .items
            .iter()
            .any(|m| m.kind == MessageKind::Progress
                && m.in_reply_to.as_deref() == Some(original.id.as_str()))
    );
    let mut reply = replies.pop().unwrap();
    reply.generation = 1;
    let Body::Delivery { content, .. } = &reply.body else {
        panic!("Expected independent delivery");
    };
    let manifest = ContentStore::new(b.store())
        .manifest(a_id, "root", content, now)
        .unwrap();
    let receiver_a = StageReceiver::new(a.clone());
    receiver_a
        .prepare_content(&incoming_a, &reply, &manifest)
        .unwrap();
    assert_eq!(content.size, 0);
    receiver_a.accept_cached_reply(&incoming_a, &reply).unwrap();
    receiver_a.accept_cached_reply(&incoming_a, &reply).unwrap();
    assert_eq!(caller_model.requests.lock().len(), 3);
    assert!(
        !a.store()
            .messages("root", None, 100)
            .unwrap()
            .items
            .iter()
            .any(|message| message.kind == MessageKind::Result)
    );
    assert!(
        a.store()
            .stage_transaction(|tx| {
                Ok(tx.query_row(
                    "SELECT completed FROM stage_outbox WHERE peer=?1 AND invocation=?2",
                    rusqlite::params![b_id.to_string(), invocation.to_string()],
                    |row| row.get::<_, bool>(0),
                )?)
            })
            .unwrap()
    );
    let mut foreign = a.actor("root").unwrap();
    foreign.id = "foreign-root".into();
    foreign.parent_id = None;
    a.store().save_actor(&foreign).unwrap();
    a.store()
        .stage_transaction(|tx| {
            tx.execute(
                "INSERT INTO stage_account_actors VALUES('foreign-root','another-account')",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    let mut forbidden = reply.clone();
    forbidden.message_id = uuid::Uuid::new_v4();
    forbidden.body = Body::BlobManifestQuery {
        access: BlobAccess::Actor {
            actor: ActorAddress {
                stage_id: a_id,
                actor_id: "foreign-root".into(),
            },
        },
        content: manifest.content,
    };
    assert!(receiver_a.handle_request(&incoming_a, &forbidden).is_err());
    a.shutdown().await;
    b.shutdown().await;
}
#[tokio::test]
async fn remote_readonly_input_is_not_owner_and_cannot_write_through_a_none_mode_root() {
    use crate::stage::{
        ActorAddress,
        identity::DeviceIdentity,
        ledger::{DeviceAuthority, StageLedger},
        protocol::{BlobRef, Body, Envelope, Origin, invocation_id},
        transport::tls,
    };
    let provider = Arc::new(ReplayProvider::new(vec![
        MockReply::Calls(vec![AgentToolCall {
            id: "remote-write".into(),
            name: "files__write_file".into(),
            arguments: json!({"path":"forbidden.txt","content":"must not be written"}),
        }]),
        MockReply::Answer("The write was denied."),
    ]));
    let (directory, system) = runtime(provider.clone()).await;
    let approved = directory.path().join("approved");
    std::fs::create_dir(&approved).unwrap();
    let approved_file = approved.join("reference.txt");
    std::fs::write(&approved_file, "remembered remote reference").unwrap();
    system
        .store()
        .stage_transaction(|tx| {
            crate::sandbox::scopes::remember(
                tx,
                &[super::super::SandboxScope::filesystem(&approved, true).unwrap()],
            )
            .unwrap();
            Ok(())
        })
        .unwrap();
    let child = system
        .create_owned("root", project_config(&system, "separate worker"))
        .unwrap();
    if let Some(MockReply::Calls(calls)) = provider.replies.lock().front_mut() {
        calls.push(AgentToolCall {
            id: "remote-read-memory".into(),
            name: "files__read_file".into(),
            arguments: json!({"path":approved_file,"start_line":null,"line_count":null}),
        });
        calls.push(AgentToolCall {
            id: "remote-write-memory".into(),
            name: "files__write_file".into(),
            arguments: json!({"path":approved.join("forbidden.txt"),"content":"denied"}),
        });
        calls.push(AgentToolCall { id:"remote-delegation".into(), name:"actors__message".into(), arguments:json!({"reply_to":null,"actor_id":child.id,"stage_id":null,"content":"Attempt delegated work"}) });
    }
    let mut host =
        DeviceIdentity::generate_for_stage(system.store().public_stage_id().unwrap()).unwrap();
    let remote = DeviceIdentity::generate().unwrap();
    let now = chrono::Utc::now().timestamp_millis();
    let root = host.initialize_group(now, now + 600000).unwrap();
    let enrollment = host
        .authorize(
            &root,
            &remote.join_request(now).unwrap(),
            now,
            now + 300000,
            false,
        )
        .unwrap();
    let ledger =
        StageLedger::initialize(system.store(), host.profile().stage_id, &root.root).unwrap();
    system
        .store()
        .stage_transaction(|tx| {
            tx.execute_batch("CREATE TABLE IF NOT EXISTS stage_account_actors(actor TEXT PRIMARY KEY REFERENCES actors(id) ON DELETE CASCADE,account TEXT NOT NULL);")?;
            tx.execute(
                "INSERT OR REPLACE INTO stage_account_actors VALUES(?1,?2)",
                rusqlite::params!["core", root.root.group_id.to_string()],
            )?;
            tx.execute(
                "INSERT OR REPLACE INTO stage_account_actors VALUES(?1,?2)",
                rusqlite::params![child.id, root.root.group_id.to_string()],
            )?;
            Ok(())
        })
        .unwrap();
    ledger
        .import_peer(
            enrollment,
            DeviceAuthority {
                read: true,
                converse: true,
                delegate: true,
                read_only: true,
            },
            now,
        )
        .unwrap();
    ledger
        .commit_generation(remote.profile().stage_id, 0, 1, uuid::Uuid::new_v4(), now)
        .unwrap();
    let acceptor = tls::server(&host, &[remote.profile()]).unwrap();
    let connector = tls::client(&remote, &host.profile()).unwrap();
    let (left, right) = tokio::io::duplex(65536);
    let (server, client) = tokio::join!(
        tls::accept(&acceptor, left),
        tls::connect(&connector, host.profile().stage_id, right)
    );
    let server = server.unwrap();
    let _client = client.unwrap();
    let _peer = tls::identify_client(&server, &[remote.profile()]).unwrap();
    let content = "Attempt a write.".to_owned();
    let frame = Envelope {
        version: 1,
        source: remote.profile().stage_id,
        target: host.profile().stage_id,
        generation: 1,
        message_id: uuid::Uuid::new_v4(),
        invocation_id: Some(invocation_id(now).unwrap()),
        in_reply_to: None,
        created_at_ms: now,
        expires_at_ms: now + 60000,
        body: Body::Invoke {
            related_message: None,
            origin: Origin::Actor {
                actor: ActorAddress {
                    stage_id: remote.profile().stage_id,
                    actor_id: "core".into(),
                },
                session_id: "core".into(),
                session_host: host.profile().stage_id,
                request_id: uuid::Uuid::new_v4(),
                ceiling: crate::stage::protocol::RemoteCeiling { read_only: true },
            },
            target: ActorAddress {
                stage_id: host.profile().stage_id,
                actor_id: child.id.clone(),
            },
            content: BlobRef {
                hash: crate::stage::digest(content.as_bytes()),
                size: content.len() as u64,
                version: 1,
            },
            attachments: vec![],
        },
    };
    let error = crate::stage::ledger::StageLedger::attach(system.store(), host.profile().stage_id)
        .accept_actor_input(remote.profile().stage_id, &frame, content, now)
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("Device cannot invoke this Actor")
    );
    assert!(
        system
            .store()
            .messages(&child.id, None, 10)
            .unwrap()
            .items
            .is_empty()
    );
    assert!(!approved.join("forbidden.txt").exists());
    system.shutdown().await;
}

fn enqueue_root(system: &ActorSystem, content: &str) -> MessageRecord {
    system
        .test_actor_input("root", content.into(), vec![], None)
        .unwrap()
}

#[tokio::test]
async fn actor_receives_pending_inputs_before_completion_and_retains_context() {
    let provider = Arc::new(ReplayProvider::new(vec![MockReply::Answer(
        "combined answer",
    )]));
    let (_directory, system) = runtime(provider.clone()).await;
    system.suspend_execution().await.unwrap();
    let first = enqueue_root(&system, "first request");
    let second = enqueue_root(&system, "second request");
    system.resume_execution().await.unwrap();
    assert_eq!(
        wait_completed(&system, &first.id).await.reply.as_deref(),
        Some("combined answer")
    );
    assert_eq!(wait_completed(&system, &second.id).await.reply, None);
    {
        let requests = provider.requests.lock();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].messages.iter().any(|row| matches!(row, AgentMessage::User { content }
            if content.as_text().contains("first request") && content.as_text().contains("second request"))));
    }
    let events = system.store().events(0, Some("root"), false, 100).unwrap();
    let streamed: String = events
        .iter()
        .filter_map(|event| match &event.event {
            ActorEvent::GenerationDelta {
                message_id,
                channel,
                text,
                ..
            } if message_id == &first.id && channel == "assistant" => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(streamed, "small delta ".repeat(50));
    assert!(!events.iter().any(|event| matches!(&event.event,
        ActorEvent::GenerationDelta { message_id, .. } if message_id == &second.id)));
    system.shutdown().await;
}

#[tokio::test]
async fn app_failures_are_model_observations_and_full_protocol_is_persisted() {
    let bad_call = AgentToolCall {
        id: "failure-call".into(),
        name: "files__unknown".into(),
        arguments: json!({}),
    };
    let provider = Arc::new(ReplayProvider::new(vec![
        MockReply::Calls(vec![bad_call]),
        MockReply::Answer("tool failed safely"),
    ]));
    let (_directory, system) = runtime(provider.clone()).await;
    let input = enqueue_root(&system, "attempt App call");
    assert_eq!(
        wait_completed(&system, &input.id).await.status,
        MessageStatus::Completed
    );
    {
        let requests = provider.requests.lock();
        assert_eq!(requests.len(), 2);
        assert!(requests[1].messages.iter().any(|row| matches!(row, AgentMessage::Tool { tool_call_id, content, .. } if tool_call_id == "failure-call" && content.contains("App error"))));
        assert!(requests[1].messages.iter().any(|row| matches!(row, AgentMessage::AssistantToolCallProtocol { reasoning_signature, .. } if reasoning_signature.as_deref() == Some("provider signature"))));
    }
    let history = system.store().history("root", 100).unwrap();
    assert!(
        history
            .iter()
            .filter_map(AgentMessage::responses_output)
            .flatten()
            .any(|item| item["encrypted_content"] == "encrypted reasoning")
    );
    system.shutdown().await;
}

#[tokio::test]
async fn shutdown_interrupts_model_wait_and_restart_never_replays_it() {
    let provider = Arc::new(ReplayProvider::new(vec![MockReply::Pending]));
    let (directory, system) = runtime(provider.clone()).await;
    let input = enqueue_root(&system, "interrupt this request");
    tokio::time::timeout(Duration::from_secs(3), provider.started.notified())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), system.shutdown())
        .await
        .unwrap();
    assert!(
        provider.dropped.load(Ordering::Acquire),
        "the outstanding provider future must be dropped"
    );
    assert_eq!(
        system.store().message(&input.id).unwrap().status,
        MessageStatus::Interrupted
    );
    let workspace = system.store().actor("root").unwrap().config.workspace;
    drop(system);
    let fresh_provider = Arc::new(ReplayProvider::new(vec![MockReply::Answer(
        "new request answer",
    )]));
    let reopened = ActorSystem::open_with_factory(
        directory.path().join("data"),
        workspace,
        runtime_config(),
        Arc::new(ReplayFactory(fresh_provider.clone())),
    )
    .await
    .unwrap();
    assert_eq!(
        reopened.store().message(&input.id).unwrap().status,
        MessageStatus::Interrupted
    );
    let fresh = enqueue_root(&reopened, "new request");
    assert_eq!(
        wait_completed(&reopened, &fresh.id).await.reply.as_deref(),
        Some("new request answer")
    );
    assert_eq!(
        fresh_provider.requests.lock().len(),
        1,
        "interrupted work must not be automatically repeated"
    );
    reopened.shutdown().await;
}

#[tokio::test]
async fn files_allow_requested_runtime_scope() {
    let provider = Arc::new(ReplayProvider::new(Vec::new()));
    let (_directory, system) = runtime(provider.clone()).await;
    let workspace = system.store().actor("root").unwrap().config.workspace;
    std::fs::write(workspace.join("inside.txt"), "authorized content\n").unwrap();
    let runtime = system.actor_state_dir("root").join("runtime.txt");
    std::fs::create_dir_all(runtime.parent().unwrap()).unwrap();
    std::fs::write(&runtime, "requested runtime content\n").unwrap();
    system
        .store()
        .stage_transaction(|tx| {
            crate::sandbox::scopes::remember(
                tx,
                &[
                    crate::sandbox::SandboxScope::filesystem(runtime.parent().unwrap(), false)
                        .unwrap(),
                ],
            )
            .unwrap();
            Ok(())
        })
        .unwrap();
    let arguments = |path: String| json!({"path":path,"start_line":null,"line_count":null});
    provider.replies.lock().extend([
        MockReply::Calls(vec![
            AgentToolCall {
                id: "inside-call".into(),
                name: "files__read_file".into(),
                arguments: arguments("inside.txt".into()),
            },
            AgentToolCall {
                id: "runtime-call".into(),
                name: "files__read_file".into(),
                arguments: arguments(runtime.display().to_string()),
            },
        ]),
        MockReply::Answer("read available data"),
    ]);
    let input = enqueue_root(&system, "read the requested paths");
    let completed = wait_completed(&system, &input.id).await;
    assert_eq!(
        completed.status,
        MessageStatus::Completed,
        "{:?}",
        completed.error
    );
    {
        let requests = provider.requests.lock();
        let follow_up = &requests[1].messages;
        assert!(follow_up.iter().any(
            |row| matches!(row, AgentMessage::Tool { tool_call_id, content, .. }
        if tool_call_id == "runtime-call" && content.contains("requested runtime content"))
        ));
        assert!(follow_up.iter().any(
            |row| matches!(row, AgentMessage::Tool { tool_call_id, content, .. }
        if tool_call_id == "inside-call" && content.contains("authorized content"))
        ));
    }
    system.shutdown().await;
}

async fn paused_runtime() -> (tempfile::TempDir, ActorSystem, Arc<ReplayProvider>) {
    let directory = crate::sandbox::test_support::scoped_directory();
    let workspace = directory.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let provider = Arc::new(ReplayProvider::new(vec![]));
    let system = ActorSystem::open_paused_with_factory(
        directory.path().join("data"),
        workspace,
        runtime_config(),
        Arc::new(ReplayFactory(provider.clone())),
    )
    .await
    .unwrap();
    system.seed_test_actor("root");

    (directory, system, provider)
}

fn project_config(system: &ActorSystem, name: &str) -> ActorConfig {
    let workspace = system.actor("root").unwrap().config.workspace.join(name);
    ActorConfig {
        name: name.into(),
        workspace,
        apps: vec!["files".into()],
        model: None,
        instructions: format!("Instructions for {name}"),
    }
}

#[tokio::test]
async fn delegated_and_direct_inputs_keep_their_original_reply_targets() {
    use super::super::{MessageKind, ReplyTarget};
    let (_directory, system, _) = paused_runtime().await;
    let child = system
        .create_owned("root", project_config(&system, "project"))
        .unwrap();
    let final_constraint =
        "Full original constraint: retain local data and never publish the report";
    let original = system
        .test_actor_input(
            "root",
            format!(
                "Delegate the project task\n{}\n{final_constraint}",
                "detail ".repeat(300)
            ),
            vec![],
            None,
        )
        .unwrap();
    system.store().claim_next("root").unwrap().unwrap();
    let delegated = system
        .message_from_actor("root", &original.id, &child.id, "Delegated work".into())
        .unwrap();
    let direct = system
        .test_actor_input(&child.id, "Separate Core-routed work".into(), vec![], None)
        .unwrap();
    system
        .store()
        .finish_message(
            &original.id,
            MessageStatus::Completed,
            Some("Work delegated"),
            None,
        )
        .unwrap();
    let newer = system
        .test_actor_input("root", "A newer independent request".into(), vec![], None)
        .unwrap();
    assert_eq!(
        system.store().claim_next("root").unwrap().unwrap().id,
        newer.id
    );
    assert_eq!(
        system.store().claim_next(&child.id).unwrap().unwrap().id,
        delegated.id
    );
    let routed = system
        .deliver_from_actor(&child.id, &delegated.id, "report", "Delegated result")
        .unwrap();
    system
        .store()
        .finish_message(&delegated.id, MessageStatus::Completed, None, None)
        .unwrap();
    assert_eq!(routed.actor_id, "root");
    assert_eq!(routed.kind, MessageKind::Progress);
    assert_eq!(routed.content, "Delegated result");
    assert_eq!(routed.in_reply_to.as_deref(), Some(original.id.as_str()));
    assert!(matches!(routed.reply_to, ReplyTarget::Actor { .. }));
    assert_eq!(
        system.store().claim_next(&child.id).unwrap().unwrap().id,
        direct.id
    );
    system
        .store()
        .finish_message(
            &direct.id,
            MessageStatus::Completed,
            Some("Direct user result"),
            None,
        )
        .unwrap();
    assert_eq!(
        system.store().message(&direct.id).unwrap().reply.as_deref(),
        Some("Direct user result")
    );
    assert_eq!(
        system
            .store()
            .message(&routed.id)
            .unwrap()
            .in_reply_to
            .as_deref(),
        Some(original.id.as_str())
    );
    assert_eq!(
        system
            .store()
            .messages("root", None, 100)
            .unwrap()
            .items
            .len(),
        3
    );
    system
        .store()
        .finish_message(
            &newer.id,
            MessageStatus::Completed,
            Some("Separate work completed"),
            None,
        )
        .unwrap();
    let active_result = system.store().claim_next("root").unwrap().unwrap();
    assert_eq!(active_result.id, routed.id);
    let root = system.actor("root").unwrap();
    let context = system.execution_context(&root, &active_result).unwrap();
    let content = system
        .message_content(&root, &active_result, &context)
        .unwrap();
    assert!(
        content.as_text().contains(final_constraint),
        "result model context must preserve original constraints beyond the API preview"
    );
    system.shutdown().await;
}

#[tokio::test]
async fn actor_identity_history_and_queued_work_survive_crash_recovery() {
    use super::super::{ApprovalDecision, ApprovalRecord};
    use crate::sandbox::SandboxScopeRequest;
    let (directory, system, provider) = paused_runtime().await;

    let config = project_config(&system, "persistent");
    let child = system.create_owned("root", config).unwrap();
    let completed = system
        .test_actor_input(
            &child.id,
            "Previously completed request".into(),
            vec![],
            None,
        )
        .unwrap();
    system.store().claim_next(&child.id).unwrap().unwrap();
    system
        .store()
        .append_history(
            &child.id,
            &completed.id,
            &[
                AgentMessage::user("previous request"),
                AgentMessage::assistant("previous answer"),
            ],
        )
        .unwrap();
    system
        .store()
        .finish_message(
            &completed.id,
            MessageStatus::Completed,
            Some("previous answer"),
            None,
        )
        .unwrap();
    let active = system
        .test_actor_input(
            &child.id,
            "Interrupted pending approval".into(),
            vec![],
            None,
        )
        .unwrap();
    system.store().claim_next(&child.id).unwrap().unwrap();
    let pending_call = call("pending-before-crash");
    system
        .store()
        .append_history(
            &child.id,
            &active.id,
            &[
                AgentMessage::user("active request"),
                protocol(vec![pending_call.clone()]),
            ],
        )
        .unwrap();
    let pending = system
        .store()
        .create_approval(ApprovalRecord {
            sudo: None,
            execution_claim: None,
            seq: 0,
            id: uuid::Uuid::new_v4().to_string(),
            message_id: active.id.clone(),
            call: pending_call,
            requested: SandboxScopeRequest::default(),
            scopes: vec![],
            previous_denials: vec![],
            decision: ApprovalDecision::Pending,
            reason: "Pending owner approval".into(),
            appeal: None,
            application: None,
            created_at_ms: chrono::Utc::now().timestamp_millis(),
            resolved_at_ms: None,
        })
        .unwrap();
    let queued = system
        .test_actor_input(&child.id, "Queued work survives".into(), vec![], None)
        .unwrap();
    let workspace = system.actor("root").unwrap().config.workspace;
    system.shutdown().await;
    drop(system);
    let reopened = ActorSystem::open_paused_with_factory(
        directory.path().join("data"),
        workspace,
        runtime_config(),
        Arc::new(ReplayFactory(provider.clone())),
    )
    .await
    .unwrap();
    let retained = reopened.actor(&child.id).unwrap();
    assert_eq!(retained.config, child.config);
    assert_eq!(retained.parent_id, child.parent_id);
    assert_eq!(retained.created_at_ms, child.created_at_ms);
    assert_eq!(
        reopened.store().message(&active.id).unwrap().status,
        MessageStatus::Interrupted
    );
    assert_eq!(
        reopened.store().message(&queued.id).unwrap().status,
        MessageStatus::Queued
    );
    assert_eq!(
        reopened.store().approval(&pending.id).unwrap().decision,
        ApprovalDecision::Cancelled
    );
    let history = reopened.store().history(&child.id, 100).unwrap();
    assert!(history.iter().any(
        |row| matches!(row, AgentMessage::Assistant { content, .. } if content == "previous answer")
    ));
    assert!(history.iter().any(
        |row| matches!(row, AgentMessage::Tool { tool_call_id, content, .. }
        if tool_call_id == "pending-before-crash" && content.contains("interrupted"))
    ));
    assert_eq!(
        reopened.store().claim_next(&child.id).unwrap().unwrap().id,
        queued.id
    );
    assert!(
        provider.requests.lock().is_empty(),
        "opening must not replay active work"
    );
    reopened.shutdown().await;
}

#[tokio::test]
async fn hosted_actors_delegate_to_account_peers_without_capability_or_recipient_lists() {
    let (_directory, system, _) = paused_runtime().await;
    let sender = system
        .create_owned("root", project_config(&system, "sender"))
        .unwrap();
    let receiver = system
        .create_owned("root", project_config(&system, "receiver"))
        .unwrap();
    let input = system
        .test_actor_input(&sender.id, "Coordinate work".into(), vec![], None)
        .unwrap();
    system.store().claim_next(&sender.id).unwrap().unwrap();
    let delegated = system
        .message_from_actor(
            &sender.id,
            &input.id,
            &receiver.id,
            "Work on your project".into(),
        )
        .unwrap();
    assert_eq!(delegated.actor_id, receiver.id);
    assert!(
        matches!(delegated.reply_to, super::super::ReplyTarget::Actor { actor_id, request_id } if actor_id == sender.id && request_id == input.id)
    );
    assert_eq!(
        system.store().claim_next(&receiver.id).unwrap().unwrap().id,
        delegated.id
    );
    let context = system
        .execution_context(&receiver, &system.store().message(&delegated.id).unwrap())
        .unwrap();
    assert!(
        context
            .sandbox_policy
            .is_path_writable(&receiver.config.workspace.join("nested/result.txt"))
    );
    system.shutdown().await;
}

#[tokio::test]
async fn removing_recipient_lists_preserves_account_ownership_including_foreign_subtrees() {
    let (_directory, system, _) = paused_runtime().await;
    system.store().stage_transaction(|tx| {
        tx.execute_batch("CREATE TABLE IF NOT EXISTS stage_account_actors(actor TEXT PRIMARY KEY REFERENCES actors(id) ON DELETE CASCADE,account TEXT NOT NULL);")?;
        tx.execute("INSERT INTO stage_account_actors VALUES('root','local-account')", [])?;
        Ok(())
    }).unwrap();
    let sender = system
        .create_owned("root", project_config(&system, "sender"))
        .unwrap();
    let peer = system
        .create_owned("root", project_config(&system, "peer"))
        .unwrap();
    let foreign = system
        .create_owned("root", project_config(&system, "foreign"))
        .unwrap();
    let child = system
        .create_owned(&foreign.id, project_config(&system, "foreign child"))
        .unwrap();
    system
        .store()
        .stage_transaction(|tx| {
            tx.execute(
                "UPDATE stage_account_actors SET account='other-account' WHERE actor=?1",
                [&foreign.id],
            )?;
            tx.execute(
                "DELETE FROM stage_account_actors WHERE actor=?1",
                [&child.id],
            )?;
            Ok(())
        })
        .unwrap();
    let visible = system.list_for_actor(&sender.id).unwrap();
    assert!(visible.iter().any(|actor| actor.id == peer.id));
    assert!(
        !visible
            .iter()
            .any(|actor| actor.id == foreign.id || actor.id == child.id)
    );
    let input = system
        .test_actor_input(
            &sender.id,
            "Coordinate this account's work".into(),
            vec![],
            None,
        )
        .unwrap();
    system.store().claim_next(&sender.id).unwrap().unwrap();
    assert!(
        system
            .message_from_actor(&sender.id, &input.id, &peer.id, "Requested work".into())
            .is_ok()
    );
    for target in [&foreign.id, &child.id] {
        assert!(
            system
                .message_from_actor(
                    &sender.id,
                    &input.id,
                    target,
                    "Forbidden cross-account work".into()
                )
                .is_err()
        );
        assert!(
            system
                .store()
                .messages(target, None, 10)
                .unwrap()
                .items
                .is_empty()
        );
    }
    system.shutdown().await;
}

#[tokio::test]
async fn deep_actor_artifact_results_are_rebound_to_parent_resource_scope() {
    use super::super::StoredResource;
    let (_directory, system, _) = paused_runtime().await;
    let mut parent_config = project_config(&system, "parent");
    parent_config.apps.push("actors".into());
    let parent = system.create_owned("root", parent_config).unwrap();
    let original = system
        .test_actor_input("root", "Produce an HTML report".into(), vec![], None)
        .unwrap();
    system.store().claim_next("root").unwrap().unwrap();
    let delegated = system
        .message_from_actor("root", &original.id, &parent.id, "Create the report".into())
        .unwrap();
    let active_parent = system.store().claim_next(&parent.id).unwrap().unwrap();
    let parent_context = system.execution_context(&parent, &active_parent).unwrap();
    let mut config = parent.config.clone();
    config.name = "report worker".into();
    config.workspace = parent.config.workspace.join("report-worker");
    let deep = system
        .create_from_actor(&parent.id, config, Default::default(), &parent_context)
        .await
        .unwrap();
    assert_eq!(deep.parent_id.as_deref(), Some(parent.id.as_str()));
    assert!(
        system
            .list_visible()
            .unwrap()
            .iter()
            .any(|actor| actor.id == deep.id)
    );
    let request = system
        .message_from_actor(&parent.id, &delegated.id, &deep.id, "Write report".into())
        .unwrap();
    system.store().claim_next(&deep.id).unwrap().unwrap();
    let resource_id = uuid::Uuid::new_v4().to_string();
    let path = system
        .actor_data_dir(&deep.id)
        .join("artifacts")
        .join(format!("{resource_id}.html"));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        "<!doctype html><title>Report</title><p>Completed</p>",
    )
    .unwrap();
    let uri = format!("/v1/actors/{}/resources/{resource_id}", deep.id);
    system
        .store()
        .register_resource_for_message(
            StoredResource {
                expires_at_ms: None,
                id: resource_id.clone(),
                actor_id: deep.id.clone(),
                path: path.clone(),
                name: "report.html".into(),
                media_type: "text/html".into(),
                size_bytes: std::fs::metadata(&path).unwrap().len(),
                uri: uri.clone(),
            },
            Some(&request.id),
        )
        .unwrap();
    let to_parent = system
        .deliver_from_actor(&deep.id, &request.id, "report", &format!("[Report]({uri})"))
        .unwrap();
    system
        .store()
        .finish_message(&request.id, MessageStatus::Completed, None, None)
        .unwrap();
    assert_eq!(to_parent.attachments.len(), 1);
    assert_eq!(to_parent.actor_id, parent.id);
    assert!(!to_parent.content.contains(&uri));
    system
        .store()
        .finish_message(
            &delegated.id,
            MessageStatus::Completed,
            Some("Report delegated"),
            None,
        )
        .unwrap();
    assert_eq!(
        system.store().claim_next(&parent.id).unwrap().unwrap().id,
        to_parent.id
    );
    let to_root = system
        .deliver_from_actor(&parent.id, &to_parent.id, "forward", &to_parent.content)
        .unwrap();
    system
        .store()
        .finish_message(&to_parent.id, MessageStatus::Completed, None, None)
        .unwrap();
    assert_eq!(to_root.actor_id, "root");
    assert_eq!(to_root.in_reply_to.as_deref(), Some(original.id.as_str()));
    assert_eq!(to_root.attachments.len(), 1);
    let root_resource = system.store().resource("root", &resource_id).unwrap();
    assert!(
        root_resource
            .path
            .starts_with(system.actor_data_dir("root"))
    );
    assert_eq!(
        std::fs::read(root_resource.path).unwrap(),
        std::fs::read(path).unwrap()
    );
    assert!(
        to_root
            .content
            .contains(&format!("/v1/actors/root/resources/{resource_id}"))
    );
    let direct = system
        .test_actor_input(
            &deep.id,
            "Direct owner question through the Actor link".into(),
            vec![],
            None,
        )
        .unwrap();
    assert_eq!(direct.actor_id, deep.id);
    assert!(matches!(
        direct.reply_to,
        super::super::ReplyTarget::Actor { .. }
    ));
    system.shutdown().await;
}

#[tokio::test]
async fn reconfiguration_stops_active_work_preserves_queue_and_keeps_one_worker() {
    {
        let provider = Arc::new(ReplayProvider::new(vec![
            MockReply::Pending,
            MockReply::Answer("queued completed after reconfiguration"),
        ]));
        let (_directory, system) = runtime(provider.clone()).await;
        let active = enqueue_root(&system, "Active request before configuration change");
        tokio::time::timeout(Duration::from_secs(3), provider.started.notified())
            .await
            .unwrap();
        let before = system.actor("root").unwrap();
        let mut invalid = before.config.clone();
        invalid.model = Some("missing-model".into());
        assert!(system.update_owned("root", invalid).await.is_err());
        assert_eq!(system.actor("root").unwrap().config, before.config);
        assert_eq!(
            system.store().message(&active.id).unwrap().status,
            MessageStatus::Running
        );
        assert!(
            !provider.dropped.load(Ordering::Acquire),
            "an invalid update must leave the old worker intact"
        );
        let queued = enqueue_root(&system, "Queued request before configuration change");
        let mut next = before.config;
        next.instructions = "Updated instructions".into();
        let update = system.update_owned("root", next.clone());
        tokio::time::timeout(Duration::from_secs(3), update)
            .await
            .unwrap()
            .unwrap();
        assert!(provider.dropped.load(Ordering::Acquire));
        assert_eq!(
            system.store().message(&active.id).unwrap().status,
            MessageStatus::Interrupted
        );
        assert_eq!(
            wait_completed(&system, &queued.id).await.reply.as_deref(),
            Some("queued completed after reconfiguration")
        );
        assert_eq!(system.actor("root").unwrap().config, next);
        {
            let requests = provider.requests.lock();
            assert_eq!(
                requests.len(),
                2,
                "exactly one execution per input, without replaying interrupted work"
            );
            assert!(
                matches!(requests[0].messages.last(), Some(AgentMessage::User { content }) if content.as_text().contains(&active.content))
            );
            assert!(
                matches!(requests[1].messages.last(), Some(AgentMessage::User { content }) if content.as_text().contains(&queued.content))
            );
        }
        system.shutdown().await;
    }
}
use async_trait::async_trait;

struct BudgetProvider {
    tokens: usize,
}

#[async_trait]
impl ModelProvider for BudgetProvider {
    async fn complete_json(
        &self,
        _: PromptRequest,
        _: ModelRequestOptions,
    ) -> Result<serde_json::Value> {
        panic!("window test must not call a provider");
    }
    async fn complete_agent_turn(
        &self,
        _: AgentTurnRequest,
        _: ModelRequestOptions,
    ) -> Result<AgentTurnStreamResult> {
        panic!("window test must not call a provider");
    }
    fn request_budget_limits(&self) -> RequestBudgetLimits {
        RequestBudgetLimits {
            context_window_tokens: self.tokens,
            auto_compact_threshold_tokens: self.tokens,
            reserved_output_tokens: 0,
        }
    }
    fn token_usage_info(&self) -> TokenUsageInfo {
        TokenUsageInfo::default()
    }
    fn model_name(&self) -> String {
        "mock-budget".to_owned()
    }
}

fn call(id: &str) -> AgentToolCall {
    AgentToolCall {
        id: id.to_owned(),
        name: "files__read_file".to_owned(),
        arguments: json!({"path": "a"}),
    }
}

fn protocol(calls: Vec<AgentToolCall>) -> AgentMessage {
    AgentMessage::assistant_tool_call_protocol_with_signed_reasoning(
        Some("reading".to_owned()),
        Some("opaque reasoning".to_owned()),
        Some("signature".to_owned()),
        calls,
    )
    .with_responses_output(Some(vec![
        json!({"type": "reasoning", "encrypted_content": "ciphertext"}),
    ]))
}

fn test_window(prompt: &str, history: &[AgentMessage], tools: &[AgentToolSpec]) -> ModelContext {
    ModelContext {
        active_input_ids: Vec::new(),
        instruction_binding: (0, crate::stage::StageId::new()),
        system: AgentMessage::system(prompt),
        history: history.to_vec(),
        tools: tools.to_vec(),
        history_complete: true,
    }
}

#[test]
fn window_preserves_complete_multi_tool_protocol_and_encrypted_reasoning() {
    let protocol = protocol(vec![call("a"), call("b")]);
    let history = vec![
        AgentMessage::user("request"),
        protocol.clone(),
        AgentMessage::tool("a", "files__read_file", "a result"),
        AgentMessage::tool("b", "files__read_file", "b result"),
        AgentMessage::user_content(AgentContent::multimodal(
            "images",
            vec![AgentContentPart::Image {
                view: None,
                path: "owned.png".to_owned(),
                media_type: "image/png".to_owned(),
                description: None,
            }],
        )),
    ];
    let window = test_window("system", &history, &[]).admitted(
        &BudgetProvider { tokens: 10_000 },
        &TokenEstimateBaseline::default(),
    );
    let window = window.unwrap();
    assert!(window.messages[2] == protocol);
    assert_eq!(
        window.messages[2].responses_output().unwrap()[0]["encrypted_content"],
        "ciphertext"
    );
    assert_eq!(window.messages.len(), 6);
}

#[tokio::test]
async fn model_context_preserves_cached_prefix_across_paged_tool_groups() {
    let (_directory, system, _) = paused_runtime().await;
    system
        .test_actor_input("root", "Inspect the files.".into(), vec![], None)
        .unwrap();
    let first = system.store().claim_next("root").unwrap().unwrap();
    let actor = system.execution_actor(&first).unwrap();
    let context = system.execution_context(&actor, &first).unwrap();
    let mut history = vec![AgentMessage::user_content(
        system.message_content(&actor, &first, &context).unwrap(),
    )];
    // Enough settled history to exercise bounded multi-page loading.
    for index in 0..150 {
        let id = format!("read-{index}");
        history.push(protocol(vec![call(&id)]));
        history.push(AgentMessage::tool(
            &id,
            "files__read_file",
            format!("result-{index}"),
        ));
    }
    system
        .store()
        .append_history("root", &first.id, &history)
        .unwrap();
    let provider = BudgetProvider { tokens: 1_000_000 };
    let baseline = TokenEstimateBaseline::default();
    let mut window = ModelContext::load(&system, &actor, &context, vec![])
        .await
        .unwrap();
    window
        .reload(&system, &first, &provider, &baseline)
        .unwrap();
    let before = window.admitted(&provider, &baseline).unwrap();
    assert!(before.messages[1..] == history);

    let pending = protocol(vec![call("pending")]);
    system
        .store()
        .append_history("root", &first.id, std::slice::from_ref(&pending))
        .unwrap();
    window
        .reload(&system, &first, &provider, &baseline)
        .unwrap();
    let unsettled = window.admitted(&provider, &baseline).unwrap();
    assert!(
        unsettled.messages == before.messages,
        "an incomplete group must not disturb the settled prefix"
    );

    let result = AgentMessage::tool("pending", "files__read_file", "pending result");
    system
        .store()
        .append_history(
            "root",
            &first.id,
            &[
                result.clone(),
                AgentMessage::assistant("Inspection complete."),
            ],
        )
        .unwrap();
    system
        .store()
        .finish_message(
            &first.id,
            MessageStatus::Completed,
            Some("Inspection complete."),
            None,
        )
        .unwrap();
    system
        .test_actor_input("root", "Inspect the next file.".into(), vec![], None)
        .unwrap();
    let second = system.store().claim_next("root").unwrap().unwrap();
    let context = system.execution_context(&actor, &second).unwrap();
    system
        .store()
        .append_history(
            "root",
            &second.id,
            &[AgentMessage::user_content(
                system.message_content(&actor, &second, &context).unwrap(),
            )],
        )
        .unwrap();
    let mut next = ModelContext::load(&system, &actor, &context, vec![])
        .await
        .unwrap();
    next.reload(&system, &second, &provider, &baseline).unwrap();
    let after = next.admitted(&provider, &baseline).unwrap();
    assert!(after.messages.starts_with(&before.messages));
    assert!(after.messages[before.messages.len()] == pending);
    assert!(after.messages[before.messages.len() + 1] == result);
    system.shutdown().await;
}

#[test]
fn oversized_window_requests_reset_without_sliding_or_mutating_history() {
    let history = vec![
        AgentMessage::user("old request"),
        protocol(vec![call("old")]),
        AgentMessage::tool("old", "files__read_file", "x".repeat(10000)),
        AgentMessage::user("current request"),
    ];
    assert!(
        test_window("system", &history, &[])
            .admitted(
                &BudgetProvider { tokens: 500 },
                &TokenEstimateBaseline::default()
            )
            .is_none()
    );
    assert_eq!(history.len(), 4);
}

#[test]
fn oversized_current_input_is_archived_for_recovery() {
    let history = vec![AgentMessage::user("z".repeat(8_000))];
    assert!(
        test_window("system", &history, &[])
            .admitted(
                &BudgetProvider { tokens: 100 },
                &TokenEstimateBaseline::default()
            )
            .is_none()
    );
}

#[test]
fn interrupted_history_cannot_replay_partial_multi_call_group() {
    let history = vec![
        AgentMessage::tool("orphan", "files__read_file", "orphaned result"),
        AgentMessage::user("request"),
        protocol(vec![call("a"), call("b")]),
        AgentMessage::tool("a", "files__read_file", "partial"),
        AgentMessage::user("next request"),
    ];
    let balanced = balanced_history(history);
    assert_eq!(balanced.len(), 2);
    assert!(
        balanced
            .iter()
            .all(|row| matches!(row, AgentMessage::User { .. }))
    );
}

#[test]
fn settled_groups_preserve_nested_inputs_images_and_original_tool_observations() {
    let outer = protocol(vec![call("outer-a"), call("outer-b")]);
    let nested = protocol(vec![call("nested")]);
    let first = AgentMessage::tool("outer-a", "files__read_file", "first result");
    let last = AgentMessage::tool("outer-b", "files__read_file", "second result");
    let result = AgentMessage::tool("nested", "files__read_file", "nested result");
    let input = AgentMessage::user("an incoming input");
    let image = AgentMessage::user_content(AgentContent::multimodal(
        "App image",
        vec![AgentContentPart::Image {
            view: None,
            path: "owned.png".into(),
            media_type: "image/png".into(),
            description: None,
        }],
    ));
    let balanced = balanced_history(vec![
        outer.clone(),
        first.clone(),
        input.clone(),
        nested.clone(),
        result.clone(),
        image.clone(),
        result.clone(),
        last.clone(),
    ]);
    assert!(balanced == vec![input, nested, result, image, outer, first, last]);
}

#[test]
fn duplicate_calls_are_rejected_before_any_effect() {
    let response = AgentTurnResponse {
        tool_calls: vec![call("same"), call("same")],
        assistant_text: None,
        final_assistant_message: None,
        reasoning_content: None,
        reasoning_signature: None,
        responses_output: None,
        raw_stream_follow_up: true,
        end_turn: false,
    };
    assert!(validate_response_calls(&response).is_err());
}

fn fixture_enrollment(
    host: &crate::stage::host::Host,
    request: &crate::stage::identity::JoinRequest,
    can_enroll: bool,
    authority: crate::stage::ledger::DeviceAuthority,
) -> crate::stage::Result<crate::stage::identity::Enrollment> {
    let now = chrono::Utc::now().timestamp_millis();
    let own = host.enrollment()?;
    let issuer =
        crate::stage::identity::verify_enrollment(&own.root, &own.chain, now, &Default::default())?;
    let enrollment = host.identity()?.authorize(
        &own,
        request,
        now,
        issuer.expires_at_ms.min(now + 180 * 24 * 60 * 60 * 1000),
        can_enroll,
    )?;
    crate::stage::ledger::StageLedger::attach(host.system.store(), host.local).import_peer(
        enrollment.clone(),
        authority,
        now,
    )?;
    Ok(enrollment)
}

#[tokio::test]
async fn valid_agent_execution_can_pass_sixty_four_steps_and_large_call_groups() {
    let mut replies: Vec<_> = (0..70)
        .map(|step| MockReply::Calls(vec![call(&format!("iteration-{step}"))]))
        .collect();
    replies.push(MockReply::Calls(
        (0..70).map(|step| call(&format!("batch-{step}"))).collect(),
    ));
    replies.push(MockReply::Answer("completed after 140 calls"));
    let provider = Arc::new(ReplayProvider::new(replies));
    let (_dir, system) = runtime(provider.clone()).await;
    let message = system
        .test_actor_input("root", "keep working".into(), vec![], None)
        .unwrap();
    let result = wait_completed(&system, &message.id).await;
    assert_eq!(result.status, MessageStatus::Completed);
    assert_eq!(result.reply.as_deref(), Some("completed after 140 calls"));
    assert_eq!(provider.requests.lock().len(), 72);
    system.shutdown().await;
}

#[tokio::test]
async fn working_history_is_not_limited_to_five_hundred_twelve_messages() {
    let provider = Arc::new(ReplayProvider::new(vec![MockReply::Answer("done")]));
    let (_dir, system) = runtime(provider.clone()).await;
    let old: Vec<_> = (0..700)
        .map(|index| AgentMessage::assistant(format!("archived-{index}")))
        .collect();
    system.store().append_history("root", "past", &old).unwrap();
    let message = system
        .test_actor_input("root", "continue".into(), vec![], None)
        .unwrap();
    assert_eq!(
        wait_completed(&system, &message.id).await.status,
        MessageStatus::Completed
    );
    assert!(
        provider.requests.lock()[0]
            .messages
            .iter()
            .any(|row| matches!(row,AgentMessage::Assistant{content,..} if content=="archived-0"))
    );
    system.shutdown().await;
}

#[tokio::test]
async fn full_context_is_cleared_with_an_archive_recovery_prompt_and_persistent_anchor() {
    let provider = Arc::new(ReplayProvider::new(vec![
        MockReply::Answer("Parser fixed. Next: verify queue ordering. Evidence: result-17."),
        MockReply::Answer("recovered"),
    ]));
    provider
        .auto_compact_threshold
        .store(40_000, Ordering::Relaxed);
    let (_dir, system) = runtime(provider.clone()).await;
    let original = AgentMessage::assistant("OLD_MODEL_WINDOW".repeat(12000));
    system
        .store()
        .append_history("root", "past", std::slice::from_ref(&original))
        .unwrap();
    let message = system
        .test_actor_input("root", "recover this task".into(), vec![], None)
        .unwrap();
    let result = wait_completed(&system, &message.id).await;
    assert_eq!(
        result.status,
        MessageStatus::Completed,
        "{:?}",
        result.error
    );
    assert_eq!(result.reply.as_deref(), Some("recovered"));
    let requests = provider.requests.lock().clone();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].tools.is_empty());
    assert!(
        requests[0].messages.contains(&original),
        "handoff must see the old window"
    );
    assert!(!requests[1].messages.contains(&original));
    assert!(requests[1].messages.iter().any(|row| matches!(row, AgentMessage::User { content }
        if content.as_text().contains("Parser fixed. Next: verify queue ordering. Evidence: result-17."))));
    assert!(
        system
            .store()
            .history("root", 100)
            .unwrap()
            .contains(&original)
    );
    let anchor = system
        .store()
        .actor("root")
        .unwrap()
        .context
        .anchor
        .unwrap();
    assert_eq!(anchor.message_id, message.id);
    assert_eq!(
        anchor.handoff.unwrap().checkpoint.as_deref(),
        Some("Parser fixed. Next: verify queue ordering. Evidence: result-17.")
    );
    assert!(system.store().context_start("root").unwrap() > 0);
    assert_eq!(
        system
            .store()
            .events(0, Some("root"), false, 100)
            .unwrap()
            .iter()
            .filter(|row| matches!(row.event, ActorEvent::GenerationStarted { .. }))
            .count(),
        1,
        "private handoff generation must not appear as a conversation response"
    );
    system.shutdown().await;
}

#[tokio::test]
async fn failed_or_invalid_handoff_never_resets_context_or_executes_tools() {
    for (replies, attempts) in [
        (
            vec![MockReply::Failure("invalid_request_error: handoff failed")],
            1,
        ),
        (vec![MockReply::Answer("")], 1),
        (vec![MockReply::Calls(vec![call("must-not-execute")])], 1),
    ] {
        let provider = Arc::new(ReplayProvider::new(replies));
        provider
            .auto_compact_threshold
            .store(40_000, Ordering::Relaxed);
        let (_dir, system) = runtime(provider.clone()).await;
        let original = AgentMessage::assistant("OLD_MODEL_WINDOW".repeat(12000));
        system
            .store()
            .append_history("root", "past", std::slice::from_ref(&original))
            .unwrap();
        let message = system
            .test_actor_input("root", "Continue the task".into(), vec![], None)
            .unwrap();
        let result = wait_completed(&system, &message.id).await;
        assert_eq!(result.status, MessageStatus::Failed);
        assert!(
            system
                .store()
                .actor("root")
                .unwrap()
                .context
                .anchor
                .is_none()
        );
        assert!(
            system
                .store()
                .history("root", 100)
                .unwrap()
                .contains(&original)
        );
        assert!(
            system
                .store()
                .tool_summaries("root", &message.id, None, 10)
                .unwrap()
                .items
                .is_empty()
        );
        assert_eq!(provider.requests.lock().len(), attempts);
        system.shutdown().await;
    }
}

#[tokio::test]
async fn oversized_handoff_keeps_complete_protocols_and_archive_locators() {
    let provider = Arc::new(ReplayProvider::new(vec![]));
    let (_dir, system) = runtime(provider).await;
    let input = enqueue_root(&system, "Verify the queue; the prior result is archived.");
    let history = vec![
        AgentMessage::user("Original goal: fix queue ordering."),
        protocol(vec![call("large-result")]),
        AgentMessage::tool(
            "large-result",
            "files__read_file",
            "LARGE-OBSERVATION".repeat(10000),
        ),
        AgentMessage::user("Latest input"),
    ];
    let tool = AgentToolSpec {
        name: "actors__read_history".into(),
        description: "Read history".into(),
        input_spec: crate::model_protocol::AgentToolInputSpec::JsonSchema {
            schema: json!({"type":"object","properties":{},"required":[],"additionalProperties":false}),
        },
    };
    let window = test_window("system", &history, &[tool]);
    let budget = BudgetProvider { tokens: 500 };
    let request = window
        .handoff_request(&input, &budget, &TokenEstimateBaseline::default())
        .unwrap();
    assert!(request.tools.is_empty());
    assert!(
        estimate_agent_turn_request(
            &request.messages,
            &request.tools,
            budget.request_budget_limits()
        )
        .within_context_window()
    );
    assert!(request.messages.contains(&history[0]));
    assert!(!request.messages.iter().any(|row| matches!(
        row,
        AgentMessage::Tool { .. } | AgentMessage::AssistantToolCallProtocol { .. }
    )));
    assert!(
        matches!(request.messages.last(), Some(AgentMessage::User { content })
        if content.as_text().contains(&input.id) && content.as_text().contains(&input.content))
    );
    assert!(
        window.history == history,
        "note admission must not mutate the old inference window"
    );
    system.shutdown().await;
}

#[tokio::test]
async fn cancelling_handoff_preserves_the_old_context() {
    let provider = Arc::new(ReplayProvider::new(vec![MockReply::Pending]));
    provider
        .auto_compact_threshold
        .store(40_000, Ordering::Relaxed);
    let (_dir, system) = runtime(provider.clone()).await;
    let original = AgentMessage::assistant("OLD_MODEL_WINDOW".repeat(12000));
    system
        .store()
        .append_history("root", "past", std::slice::from_ref(&original))
        .unwrap();
    let message = system
        .test_actor_input("root", "Continue the task".into(), vec![], None)
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while provider.pending.load(Ordering::Acquire) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    system.cancel("root", &message.id).unwrap();
    assert_eq!(
        wait_completed(&system, &message.id).await.status,
        MessageStatus::Cancelled
    );
    assert!(provider.dropped.load(Ordering::Acquire));
    assert!(
        system
            .store()
            .actor("root")
            .unwrap()
            .context
            .anchor
            .is_none()
    );
    assert!(
        system
            .store()
            .history("root", 100)
            .unwrap()
            .contains(&original)
    );
    system.shutdown().await;
}

#[test]
fn window_admission_uses_observed_input_bias_without_reducing_estimates() {
    let history = vec![AgentMessage::user("x".repeat(800))];
    let low = TokenEstimateBaseline {
        estimated_input_tokens: 100,
        observed_input_tokens: Some(20),
    };
    assert!(
        test_window("system", &history, &[])
            .admitted(&BudgetProvider { tokens: 500 }, &low)
            .is_some()
    );
    let high = TokenEstimateBaseline {
        estimated_input_tokens: 100,
        observed_input_tokens: Some(450),
    };
    assert!(
        test_window("system", &history, &[])
            .admitted(&BudgetProvider { tokens: 500 }, &high)
            .is_none()
    );
}

#[tokio::test]
async fn token_calibration_survives_restart_and_is_bound_to_the_model() {
    let provider = Arc::new(ReplayProvider::new(vec![
        MockReply::Answer("done"),
        MockReply::Answer("next"),
    ]));
    provider.usage_bias.store(1234, Ordering::Relaxed);
    let (dir, system) = runtime(provider.clone()).await;
    let first = system
        .test_actor_input("root", "first".into(), vec![], None)
        .unwrap();
    assert_eq!(
        wait_completed(&system, &first.id).await.status,
        MessageStatus::Completed
    );
    let state = system.store().actor("root").unwrap().context;
    assert_eq!(
        state.token_estimate.observed_input_tokens,
        Some(state.token_estimate.estimated_input_tokens + 1234)
    );
    let workspace = system.actor("root").unwrap().config.workspace;
    system.shutdown().await;
    drop(system);
    let reopened = ActorSystem::open_with_factory(
        dir.path().join("data"),
        workspace,
        runtime_config(),
        Arc::new(ReplayFactory(provider.clone())),
    )
    .await
    .unwrap();
    assert_eq!(reopened.store().actor("root").unwrap().context, state);
    assert_eq!(
        ModelContext::baseline(&reopened, "root", "another-model").unwrap(),
        TokenEstimateBaseline::default()
    );
    provider.usage_bias.store(0, Ordering::Relaxed);
    let second = reopened
        .test_actor_input("root", "second".into(), vec![], None)
        .unwrap();
    assert_eq!(
        wait_completed(&reopened, &second.id).await.status,
        MessageStatus::Completed
    );
    assert_eq!(
        reopened
            .store()
            .actor("root")
            .unwrap()
            .context
            .token_estimate,
        state.token_estimate,
        "missing usage must not reuse a previous response as a new observation"
    );
    reopened.shutdown().await;
}

#[tokio::test]
async fn actor_archive_is_own_actor_scoped_paged_and_has_no_provider_reasoning() {
    use crate::{actor_app::ActorApp, app::App};
    let provider = Arc::new(ReplayProvider::new(vec![MockReply::Pending]));
    let (_dir, system) = runtime(provider.clone()).await;
    let child = system
        .create_owned("root", project_config(&system, "other actor"))
        .unwrap();
    system
        .store()
        .append_history(
            &child.id,
            "other-input",
            &[AgentMessage::user("OTHER_ACTOR_SECRET")],
        )
        .unwrap();
    system
        .store()
        .append_history(
            "root",
            "root-input",
            &[AgentMessage::user("根历史内容".repeat(3000))],
        )
        .unwrap();
    let message = system
        .test_actor_input("root", "wait".into(), vec![], None)
        .unwrap();
    provider.started.notified().await;
    let actor = system.actor("root").unwrap();
    let context = system.execution_context(&actor, &message).unwrap();
    let mut app = ActorApp::new("root".into(), system.clone());
    let mut query = AgentToolCall {
        id: "history-query".into(),
        name: "read_history".into(),
        arguments: json!({"cursor":null,"message_id":"other-input","limit":10}),
    };
    let foreign = app.execute_tool(&query, &context).await.unwrap();
    assert!(foreign.payload["entries"].as_array().unwrap().is_empty());
    query.arguments = json!({"cursor":null,"message_id":"root-input","limit":10});
    let first = app.execute_tool(&query, &context).await.unwrap();
    assert!(first.payload["next_cursor"].is_string());
    assert!(!first.payload.to_string().contains("OTHER_ACTOR_SECRET"));
    query.arguments["cursor"] = first.payload["next_cursor"].clone();
    let second = app.execute_tool(&query, &context).await.unwrap();
    assert!(second.payload["entries"][0]["offset"].as_u64().unwrap() > 0);
    system.shutdown().await;
}

#[tokio::test]
async fn runner_prompt_keeps_identity_stable_across_request_authority() {
    let (_directory, system) = runtime(Arc::new(ReplayProvider::new(vec![]))).await;
    let mut actor = system.actor("root").unwrap();
    actor.config.instructions = "Keep the assigned project context.".into();
    std::fs::write(
        actor.config.workspace.join("AGENTS.md"),
        "Worker project instructions fixture.",
    )
    .unwrap();
    let context = system.owner_app_context().unwrap();
    let mut previous_prompt = None;
    for read_only in [false, true] {
        actor.remote_read_only = read_only;
        let assembled = SystemPrompt::load(&system, &actor, &context).await.unwrap();
        if cfg!(target_os = "windows") {
            let name = format!("os/{}-patch.md", std::env::consts::OS);
            assert!(
                assembled.files.iter().any(|file| file.name == name),
                "execution Actors must retain their platform patch"
            );
        }
        let prompt = assembled.render();
        let expected = format!(
            "Actor name: {}\nActor ID: {}\nWorkspace: {}\n",
            actor.config.name,
            actor.id,
            actor.config.workspace.display(),
        );
        assert!(prompt.starts_with(&expected));
        if let Some(previous) = &previous_prompt {
            assert_eq!(&prompt, previous);
        }
        previous_prompt = Some(prompt.clone());
        assert_shared_actor_instructions(&prompt);
        assert!(!prompt.contains("You are DaatLocus,"));
        assert!(!prompt.contains("{{account_name}}"));
        assert_eq!(prompt.matches(&actor.config.instructions).count(), 1);
        assert_eq!(
            prompt
                .matches("Worker project instructions fixture.")
                .count(),
            1
        );
    }
    system.shutdown().await;
}

#[tokio::test]
async fn runner_preserves_cached_prefix_across_input_boundaries() {
    for actor_id in ["root"] {
        for reset in [false, true] {
            let provider = Arc::new(ReplayProvider::new(vec![
                MockReply::Answer("First response."),
                MockReply::Answer("Second response."),
                MockReply::Answer("Third response."),
            ]));
            let (_directory, system) = runtime(provider.clone()).await;
            let host = crate::stage::host::Host::load(system.clone()).unwrap();
            host.initialize(true).unwrap();
            for (index, content) in ["First input.", "Second input.", "Third input."]
                .into_iter()
                .enumerate()
            {
                let input = system
                    .test_actor_input(actor_id, content.into(), vec![], None)
                    .unwrap();
                let result = wait_completed(&system, &input.id).await;
                assert_eq!(
                    result.status,
                    MessageStatus::Completed,
                    "{:?}",
                    result.error
                );
                if reset && index == 0 {
                    system
                        .store()
                        .reset_model_context(
                            actor_id,
                            &input.id,
                            "First response delivered. Continue with later inputs.",
                        )
                        .unwrap();
                }
            }
            let requests = provider.requests.lock().clone();
            assert_eq!(requests.len(), 3);
            for pair in requests[usize::from(reset)..].windows(2) {
                assert!(
                    pair[1].messages.starts_with(&pair[0].messages),
                    "{actor_id}, reset={reset}: new inputs must preserve the previous request as a cacheable prefix"
                );
                assert_eq!(
                    serde_json::to_value(&pair[0].tools).unwrap(),
                    serde_json::to_value(&pair[1].tools).unwrap()
                );
            }
            system.shutdown().await;
        }
    }
}

#[tokio::test]
async fn runner_keeps_runtime_ids_out_of_ordinary_inputs() {
    let provider = Arc::new(ReplayProvider::new(vec![MockReply::Answer("Done.")]));
    let (_directory, system) = runtime(provider.clone()).await;
    let input = system
        .test_actor_input("root", "Answer from this context.".into(), vec![], None)
        .unwrap();
    let completed = wait_completed(&system, &input.id).await;
    assert_eq!(completed.status, MessageStatus::Completed);
    assert_eq!(completed.reply.as_deref(), Some("Done."));
    {
        let requests = provider.requests.lock();
        assert!(
            !serde_json::to_string(&requests[0].messages)
                .unwrap()
                .contains(&input.id),
            "ordinary inputs must not expose their runtime correlation ID"
        );
        assert!(requests[0].messages.iter().any(|message| matches!(message, AgentMessage::User {content} if content.as_text().ends_with("Answer from this context."))));
    }
    system.shutdown().await;
}

#[tokio::test]
async fn builtin_skill_metadata_is_discovered_by_workers_but_not_core() {
    let (directory, system) = runtime(Arc::new(ReplayProvider::new(vec![]))).await;
    let temporary = crate::runtime_temp::RuntimeTemp::create(
        &directory
            .path()
            .canonicalize()
            .unwrap()
            .join("fixture-runtime"),
    )
    .unwrap();
    let content = b"---\nname: builtin-prompt-fixture\ndescription: Discover a bundled procedure.\n---\n\nDetailed body stays on disk.\n";
    let path = temporary
        .publish(
            std::path::Path::new("skills/prompt-fixture/SKILL.md"),
            content,
        )
        .unwrap();
    let mut context = system.owner_app_context().unwrap();
    context.sandbox_policy.filesystem.full_disk_read = false;
    context.sandbox_policy.filesystem.full_disk_write = false;
    context.sandbox_policy.filesystem.readable_roots.clear();
    context.sandbox_policy.filesystem.writable_roots.clear();
    context.sandbox_policy.filesystem.deny_read_paths = vec![directory.path().into()];
    context.sandbox_policy.builtin_resources =
        vec![crate::builtin_resources::BuiltinResource { path, content }];
    let mut worker = system.actor("root").unwrap();
    for read_only in [false, true] {
        worker.remote_read_only = read_only;
        let prompt = SystemPrompt::load(&system, &worker, &context)
            .await
            .unwrap()
            .render();
        assert_eq!(prompt.matches("builtin-prompt-fixture").count(), 1);
        assert_shared_actor_instructions(&prompt);
        assert!(!prompt.contains("You are DaatLocus,"));
        assert!(!prompt.contains("Detailed body stays on disk."));
    }
    let core = system.actor("core").unwrap();
    let prompt = SystemPrompt::load(&system, &core, &context)
        .await
        .unwrap()
        .render();
    assert!(!prompt.contains("builtin-prompt-fixture"));
    assert_shared_actor_instructions(&prompt);
    assert_eq!(prompt.matches("You are DaatLocus,").count(), 1);
    system.shutdown().await;
}

struct RuntimeScopeFixture {
    executions: Arc<std::sync::atomic::AtomicUsize>,
    denied_target: Option<std::path::PathBuf>,
    permission_denied: bool,
    output: String,
}
#[async_trait]
impl crate::app::App for RuntimeScopeFixture {
    fn id(&self) -> crate::app::AppId {
        crate::app::AppId::new("fixture").unwrap()
    }
    fn tool_specs(&self) -> Vec<crate::app::AppToolSpec> {
        vec![crate::app::AppToolSpec {
            name: "inspect".into(),
            description: "Synthetic scope fixture".into(),
            input_schema: crate::schema_utils::model_schema_for::<crate::app::EmptyAppArgs>(),
        }]
    }
    async fn execute_tool(
        &mut self,
        _: &AgentToolCall,
        context: &AppToolExecutionContext,
    ) -> Result<crate::app::AppToolExecutionResult> {
        self.executions.fetch_add(1, Ordering::SeqCst);
        if self.permission_denied {
            return Err(super::super::approval::AutomaticReviewDenied(
                super::super::approval::ApprovalRejection {
                    approval_id: "denied-fixture-scope".into(),
                    stage_id: "fixture-stage".into(),
                    reason: "Owner declined the exact scope".into(),
                },
            )
            .into());
        }
        if let Some(target) = &self.denied_target {
            context
                .sandbox_policy
                .ensure_path_writable(target, "fixture output")?;
        }
        Ok(crate::app::AppToolExecutionResult::new(
            "Synthetic output",
            json!({"output":self.output,"exit_code":0,"running":true}),
            Some(self.output.clone()),
        ))
    }
}

#[tokio::test]
async fn runtime_scope_reference_preserves_generic_output_and_never_replays_app() {
    let (directory, system, provider) = paused_runtime().await;
    provider
        .review_replies
        .lock()
        .push_back(json!({"decision":"approve","reason":"requested fixture scope"}));
    let queued = system
        .test_actor_input("root", "Inspect fixture".into(), vec![], None)
        .unwrap();
    let message = system.store().claim_next("root").unwrap().unwrap();
    assert_eq!(message.id, queued.id);
    let actor = system.execution_actor(&message).unwrap();
    let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let output = "prefix result\nchild: Permission denied\nsuffix result";
    let mut apps = AppManager::new(vec![Box::new(RuntimeScopeFixture {
        executions: counter.clone(),
        denied_target: None,
        permission_denied: false,
        output: output.into(),
    })])
    .unwrap();
    let call = AgentToolCall {
        id: "generic-scope-output".into(),
        name: "fixture__inspect".into(),
        arguments: json!({}),
    };
    let mut history = vec![];
    execute_call(
        &system,
        &actor,
        &message,
        &ActorControl::default(),
        &mut apps,
        &call,
        &mut history,
    )
    .await
    .unwrap();
    let summary = system
        .store()
        .tool_summaries(&actor.id, &message.id, None, 10)
        .unwrap()
        .items
        .remove(0);
    let record = system.store().tool_result(&summary.id).unwrap();
    assert_eq!(
        record.payload,
        json!({"output":output,"exit_code":0,"running":true})
    );
    assert!(record.model_content.starts_with(output));
    let metadata = record
        .model_content
        .lines()
        .find_map(|line| {
            serde_json::from_str::<serde_json::Value>(line)
                .ok()
                .and_then(|v| v.get("permission_reference").cloned())
        })
        .unwrap();
    assert_eq!(metadata["tool_result_id"], record.id);
    assert!(metadata.get("approval_id").is_none());
    let root = directory.path().join("additional");
    std::fs::create_dir(&root).unwrap();
    let context = system.execution_context(&actor, &message).unwrap();
    let mut actors = crate::actor_app::ActorApp::new(actor.id.clone(), system.clone());
    let request = AgentToolCall {
        id: "scope-application".into(),
        name: "appeal_approval".into(),
        arguments: json!({"stage_id":metadata["stage_id"],"approval_id":null,"tool_result_id":metadata["tool_result_id"],"readable_roots":[],"writable_roots":[root],"explanation":"Write the exact requested additional fixture"}),
    };
    let approved = crate::app::App::execute_tool(&mut actors, &request, &context)
        .await
        .unwrap();
    assert_eq!(approved.payload["approval"]["decision"], "approved");
    assert_eq!(approved.payload["executed"], false);
    assert_eq!(counter.load(Ordering::SeqCst), 1);
    assert_eq!(
        system.store().tool_result(&record.id).unwrap().payload,
        record.payload
    );
    let mut refused_app = AppManager::new(vec![Box::new(RuntimeScopeFixture {
        executions: counter.clone(),
        denied_target: None,
        permission_denied: true,
        output: String::new(),
    })])
    .unwrap();
    let refused_call = AgentToolCall {
        id: "explicit-refusal".into(),
        ..call
    };
    execute_call(
        &system,
        &actor,
        &message,
        &ActorControl::default(),
        &mut refused_app,
        &refused_call,
        &mut history,
    )
    .await
    .unwrap();
    let refused = system
        .store()
        .tool_summaries(&actor.id, &message.id, None, 10)
        .unwrap()
        .items
        .into_iter()
        .find(|record| record.call_id == refused_call.id)
        .unwrap();
    let refused = system.store().tool_result(&refused.id).unwrap();
    assert_eq!(refused.payload["approval"]["decision"], "denied");
    assert!(
        system
            .store()
            .native_tool_result(&refused.id, &actor.id, &message.id)
            .is_err(),
        "a known denied-result UUID must not become a new scope application"
    );
    let mut refused_request = request.clone();
    refused_request.arguments["tool_result_id"] = json!(refused.id);
    assert!(
        crate::app::App::execute_tool(&mut actors, &refused_request, &context)
            .await
            .is_err()
    );
    assert_eq!(counter.load(Ordering::SeqCst), 2);
    system.shutdown().await;
}

#[tokio::test]
async fn runtime_scope_typed_app_denial_and_bounded_output_keep_consumable_references() {
    let (directory, system, _) = paused_runtime().await;
    system
        .test_actor_input("root", "Inspect fixture".into(), vec![], None)
        .unwrap();
    let message = system.store().claim_next("root").unwrap().unwrap();
    let actor = system.execution_actor(&message).unwrap();
    let target = directory.path().join("outside");
    std::fs::create_dir(&target).unwrap();
    let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut apps = AppManager::new(vec![Box::new(RuntimeScopeFixture {
        executions: counter.clone(),
        denied_target: Some(target.clone()),
        permission_denied: false,
        output: String::new(),
    })])
    .unwrap();
    let call = AgentToolCall {
        id: "typed-internal-denial".into(),
        name: "fixture__inspect".into(),
        arguments: json!({}),
    };
    execute_call(
        &system,
        &actor,
        &message,
        &ActorControl::default(),
        &mut apps,
        &call,
        &mut vec![],
    )
    .await
    .unwrap();
    let summary = system
        .store()
        .tool_summaries(&actor.id, &message.id, None, 10)
        .unwrap()
        .items
        .remove(0);
    let result = system.store().tool_result(&summary.id).unwrap();
    assert!(result.error.is_some());
    assert!(
        result
            .model_content
            .starts_with(result.error.as_ref().unwrap())
    );
    let reference = result
        .model_content
        .lines()
        .find_map(|line| {
            serde_json::from_str::<serde_json::Value>(line)
                .ok()
                .and_then(|v| v.get("permission_reference").cloned())
        })
        .unwrap();
    let approval = system
        .store()
        .approval(reference["approval_id"].as_str().unwrap())
        .unwrap();
    assert_eq!(
        approval.decision,
        super::super::ApprovalDecision::ApplicationRequired
    );
    assert_eq!(
        approval.requested.writable_roots,
        vec![target.canonicalize().unwrap()]
    );
    assert_eq!(counter.load(Ordering::SeqCst), 1);
    let huge_call = AgentToolCall {
        id: "large-output-reference".into(),
        ..call
    };
    let result_id = system
        .store()
        .tool_started(&actor.id, &message.id, &huge_call)
        .unwrap();
    let mut context = system.execution_context(&actor, &message).unwrap();
    context.tool_output_max_tokens = 64;
    let raw = "unaltered fixture output ".repeat(3000);
    let mut huge = crate::app::AppToolExecutionResult::new(
        "Large result",
        json!({"output":raw}),
        Some(raw.clone()),
    );
    super::super::runtime_permissions::attach_reference(
        &system, &actor, &message, &huge_call, &context, &result_id, &mut huge, None,
    )
    .unwrap();
    finish_tool_result(
        &system,
        &actor,
        &message,
        &huge_call,
        &context,
        huge,
        &mut vec![],
    )
    .unwrap();
    let saved = system.store().tool_result(&result_id).unwrap();
    assert_eq!(saved.payload["output"], raw);
    assert!(saved.model_content.contains(&result_id));
    assert!(saved.model_content.contains("permission_reference"));
    system.shutdown().await;
}

#[path = "message_tests.rs"]
mod message_tests;

fn sudo_write_call(id: &str, path: &std::path::Path, approval: Option<&str>) -> AgentToolCall {
    AgentToolCall {
        id: id.into(),
        name: "sudo__execute".into(),
        arguments: json!({
            "tool":"files__write_file", "arguments_json":json!({"path":path,"content":"reviewed host effect"}).to_string(),
            "reason":"Write the report requested by the Owner", "restart_native":false, "approval_id":approval,
        }),
    }
}

#[tokio::test]
async fn native_capture_failures_preserve_output_and_late_scope_evidence_in_saved_results() {
    let (directory, system, _) = paused_runtime().await;
    system
        .test_actor_input("root", "Check package setup".into(), vec![], None)
        .unwrap();
    let message = system.store().claim_next("root").unwrap().unwrap();
    let actor = system.execution_actor(&message).unwrap();
    let context = system.execution_context(&actor, &message).unwrap();
    let call = AgentToolCall {
        id: "native-evidence".into(),
        name: "terminal__terminal_exec".into(),
        arguments: json!({}),
    };
    let id = system
        .store()
        .tool_started(&actor.id, &message.id, &call)
        .unwrap();
    let late = directory.path().join("package-cache");
    std::fs::create_dir(&late).unwrap();
    let mut denials=(0..32).map(|n| json!({"resource":directory.path().join(format!("probe-{n}")),"resourceType":"file","accessType":"write"})).collect::<Vec<_>>();
    denials.push(json!({"resource":late,"resourceType":"directory","accessType":"write"}));
    denials.push(
        json!({"resource":"package-activation","resourceType":"capability","accessType":"execute"}),
    );
    let report=crate::sandbox::report_from_capture_document(&serde_json::to_vec(&json!({"denials":denials,"summary":{"deniedResourcesTruncated":true,"totalDenials":40}})).unwrap());
    let error: miette::Report = crate::sandbox::ProcessFailure {
        message: "original native failure".into(),
        report: report.clone(),
    }
    .into();
    let mut result = super::super::runtime_permissions::error_result(&system, &error);
    super::super::runtime_permissions::attach_reference(
        &system,
        &actor,
        &message,
        &call,
        &context,
        &id,
        &mut result,
        Some(&error),
    )
    .unwrap();
    finish_tool_result(
        &system,
        &actor,
        &message,
        &call,
        &context,
        result,
        &mut vec![],
    )
    .unwrap();
    let saved = system.store().tool_result(&id).unwrap();
    assert_eq!(saved.error.as_deref(), Some("original native failure"));
    assert_eq!(saved.payload["sandbox"], json!(report));
    let reference = saved
        .model_content
        .lines()
        .find_map(|line| {
            serde_json::from_str::<serde_json::Value>(line)
                .ok()
                .and_then(|value| value.get("permission_reference").cloned())
        })
        .unwrap();
    let approval = system
        .store()
        .approval(reference["approval_id"].as_str().unwrap())
        .unwrap();
    assert!(
        approval
            .requested
            .writable_roots
            .contains(&late.canonicalize().unwrap())
    );
    assert_eq!(
        approval.requested.writable_roots.len(),
        33,
        "non-filesystem observations cannot turn into grants"
    );
    system.shutdown().await;
}
fn sudo_fixture_apps() -> AppManager {
    AppManager::new(vec![
        Box::new(crate::sudo_app::SudoApp),
        Box::new(crate::files_app::FilesApp::new(std::env::temp_dir(), false)),
    ])
    .unwrap()
}

#[tokio::test]
async fn sudo_review_buffers_incoming_input_and_executes_once_without_device_scope_memory() {
    let (directory, system, provider) = paused_runtime().await;
    provider
        .review_replies
        .lock()
        .push_back(json!({"decision":"approve","reason":"Owner requested the report"}));
    provider.review_blocked.store(true, Ordering::Release);
    provider
        .replies
        .lock()
        .push_back(MockReply::Answer("Received the new instructions"));
    system
        .test_actor_input("root", "Write the report".into(), vec![], None)
        .unwrap();
    let message = system.store().claim_next("root").unwrap().unwrap();
    let actor = system.execution_actor(&message).unwrap();
    let before = system.store().sandbox_scopes().unwrap();
    let path = directory.path().join("report.txt");
    assert!(
        !system
            .execution_context(&actor, &message)
            .unwrap()
            .sandbox_policy
            .is_path_writable(&path)
    );
    let mut apps = sudo_fixture_apps();
    let call = sudo_write_call("sudo-write", &path, None);
    let control = ActorControl::default();
    let mut history = vec![];
    let execution = execute_call(
        &system,
        &actor,
        &message,
        &control,
        &mut apps,
        &call,
        &mut history,
    );
    let incoming_message = async {
        while provider.review_requests.lock().is_empty() {
            tokio::task::yield_now().await;
        }
        assert!(!path.exists(), "effects must wait for Auto");
        let incoming = system
            .test_actor_input(
                "root",
                "Keep the requested report concise".into(),
                vec![],
                None,
            )
            .unwrap();
        assert_eq!(
            system.store().message(&incoming.id).unwrap().status,
            MessageStatus::Queued
        );
        assert!(provider.requests.lock().is_empty());
        assert!(!path.exists(), "effects must still wait for Auto");
        provider.review_release.notify_one();
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(execution, incoming_message)
    })
    .await
    .unwrap();
    result.unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "reviewed host effect"
    );
    assert_eq!(provider.review_requests.lock().len(), 1);
    assert_eq!(system.store().sandbox_scopes().unwrap(), before);
    let record = system
        .store()
        .approvals(Some("root"), None, 10)
        .unwrap()
        .items
        .remove(0);
    assert!(record.execution_claim.is_some());
    assert!(record.appeal.is_none());
    let repeat = sudo_write_call("sudo-replay", &path, Some(&record.id));
    execute_call(
        &system,
        &actor,
        &message,
        &control,
        &mut apps,
        &repeat,
        &mut history,
    )
    .await
    .unwrap();
    assert_eq!(provider.review_requests.lock().len(), 1);
    let summaries = system
        .store()
        .tool_summaries("root", &message.id, None, 10)
        .unwrap();
    let repeated = summaries
        .items
        .iter()
        .find(|tool| tool.call_id == "sudo-replay")
        .unwrap();
    assert!(
        system
            .store()
            .tool_result(&repeated.id)
            .unwrap()
            .error
            .is_some()
    );
    system.shutdown().await;
}

#[tokio::test]
async fn sudo_denial_goes_to_core_and_consent_requires_an_explicit_unchanged_invocation() {
    let (directory, system, provider) = paused_runtime().await;
    provider
        .review_replies
        .lock()
        .push_back(json!({"decision":"deny","reason":"Core should confirm authorization"}));
    system
        .test_actor_input("root", "Write the report".into(), vec![], None)
        .unwrap();
    let message = system.store().claim_next("root").unwrap().unwrap();
    let actor = system.execution_actor(&message).unwrap();
    let path = directory.path().join("report.txt");
    let mut apps = sudo_fixture_apps();
    let control = ActorControl::default();
    let mut history = vec![];
    let call = sudo_write_call("sudo-denied", &path, None);
    execute_call(
        &system,
        &actor,
        &message,
        &control,
        &mut apps,
        &call,
        &mut history,
    )
    .await
    .unwrap();
    assert!(!path.exists());
    let record = system
        .store()
        .approvals(Some("root"), None, 10)
        .unwrap()
        .items
        .remove(0);
    assert_eq!(record.decision, super::super::ApprovalDecision::Denied);
    assert!(record.appeal.is_some());
    assert!(record.execution_claim.is_none());
    let mut elevated = sudo_write_call("sudo-elevation-substitution", &path, Some(&record.id));
    elevated.name = "sudo__execute_as_admin".into();
    execute_call(
        &system,
        &actor,
        &message,
        &control,
        &mut apps,
        &elevated,
        &mut history,
    )
    .await
    .unwrap();
    assert!(!path.exists());
    assert!(
        system
            .store()
            .approval(&record.id)
            .unwrap()
            .execution_claim
            .is_none()
    );
    let retry = sudo_write_call("sudo-pending", &path, Some(&record.id));
    execute_call(
        &system,
        &actor,
        &message,
        &control,
        &mut apps,
        &retry,
        &mut history,
    )
    .await
    .unwrap();
    assert_eq!(provider.review_requests.lock().len(), 1);
    let core_input = system.store().claim_next("core").unwrap().unwrap();
    let core = system.actor("core").unwrap();
    let context = system.execution_context(&core, &core_input).unwrap();
    system
        .resolve_appeal_from_core(
            context.actor_call.as_ref().unwrap(),
            &record.id,
            super::super::ApprovalDecision::Approved,
            "The Owner authorized this report",
        )
        .await
        .unwrap();
    assert!(!path.exists(), "consent never replays the source effect");
    let forged_path = directory.path().join("unapproved.txt");
    let forged = sudo_write_call("sudo-forged", &forged_path, Some(&record.id));
    execute_call(
        &system,
        &actor,
        &message,
        &control,
        &mut apps,
        &forged,
        &mut history,
    )
    .await
    .unwrap();
    assert!(!forged_path.exists());
    assert!(
        system
            .store()
            .approval(&record.id)
            .unwrap()
            .execution_claim
            .is_none()
    );
    // The decision notification remains a separate durable input; execute the
    // explicit retry under this still-active original request's authority.
    let retry = sudo_write_call("sudo-approved", &path, Some(&record.id));
    execute_call(
        &system,
        &actor,
        &message,
        &control,
        &mut apps,
        &retry,
        &mut history,
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(path).unwrap(),
        "reviewed host effect"
    );
    assert_eq!(provider.review_requests.lock().len(), 1);
    system.shutdown().await;
}

#[tokio::test]
async fn sudo_rejects_hard_authority_and_invalid_targets_before_automatic_review() {
    let (directory, system, provider) = paused_runtime().await;
    system
        .test_actor_input("root", "Write a report".into(), vec![], None)
        .unwrap();
    let message = system.store().claim_next("root").unwrap().unwrap();
    let actor = system.execution_actor(&message).unwrap();
    let context = system.execution_context(&actor, &message).unwrap();
    let path = directory.path().join("unauthorized.txt");
    let call = sudo_write_call("sudo-hard-authority", &path, None);
    let control = ActorControl::default();
    let mut readonly = context.clone();
    readonly.actor_call.as_mut().unwrap().remote_read_only = true;
    assert!(super::super::sudo::unrestricted_context(&readonly).is_err());
    let mut remote = message.clone();
    remote.remote_authority = Some(super::super::RemoteAuthority {
        read_only: false,
        peer: system.store().public_stage_id().unwrap(),
        origin_actor: "root".into(),
        reply_for: None,
    });
    let (args, invocation) =
        crate::sudo_app::SudoApp::request(&call, context.execution_cwd.clone()).unwrap();
    assert!(
        super::super::sudo::review(
            &system, &actor, &remote, &control, &context, &call, args, invocation
        )
        .await
        .is_err()
    );
    let mut invalid = call.clone();
    invalid.arguments["arguments_json"] = json!("{\"unexpected\":true}");
    execute_call(
        &system,
        &actor,
        &message,
        &control,
        &mut sudo_fixture_apps(),
        &invalid,
        &mut vec![],
    )
    .await
    .unwrap();
    assert!(provider.review_requests.lock().is_empty());
    assert!(
        system
            .store()
            .approvals(Some("root"), None, 10)
            .unwrap()
            .items
            .is_empty()
    );
    assert!(!path.exists());
    system.shutdown().await;
}

#[tokio::test]
async fn sudo_cancelled_while_waiting_for_auto_never_executes() {
    let (directory, system, provider) = paused_runtime().await;
    provider.review_blocked.store(true, Ordering::Release);
    provider
        .review_replies
        .lock()
        .push_back(json!({"decision":"approve","reason":"Owner requested it"}));
    system
        .test_actor_input("root", "Write a report".into(), vec![], None)
        .unwrap();
    let message = system.store().claim_next("root").unwrap().unwrap();
    let actor = system.execution_actor(&message).unwrap();
    let path = directory.path().join("cancelled.txt");
    let call = sudo_write_call("sudo-cancelled", &path, None);
    let control = ActorControl::default();
    let mut apps = sudo_fixture_apps();
    let mut history = vec![];
    let execution = execute_call(
        &system,
        &actor,
        &message,
        &control,
        &mut apps,
        &call,
        &mut history,
    );
    let cancellation = async {
        while provider.review_requests.lock().is_empty() {
            tokio::task::yield_now().await;
        }
        control.stop.store(true, Ordering::Release);
        provider.review_release.notify_one();
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(execution, cancellation)
    })
    .await
    .unwrap();
    assert!(result.is_err());
    assert!(!path.exists());
    for record in system
        .store()
        .approvals(Some("root"), None, 10)
        .unwrap()
        .items
    {
        assert!(record.execution_claim.is_none());
    }
    system.shutdown().await;
}

#[cfg(windows)]
#[tokio::test]
#[ignore = "requires native MXC/PSEC and a registered packaged winget client; runs --version only"]
async fn sudo_runs_packaged_winget_without_uac_after_native_container_refuses_it() {
    assert!(
        crate::sandbox::test_support::native_sandbox_available(),
        "this opt-in probe requires native isolation"
    );
    let alias = std::path::PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap())
        .join("Microsoft/WindowsApps/winget.exe");
    let expected = std::process::Command::new(&alias)
        .arg("--version")
        .output()
        .expect("registered packaged winget prerequisite");
    assert!(expected.status.success());
    let expected = String::from_utf8(expected.stdout).unwrap();
    let (_directory, system, provider) = paused_runtime().await;
    provider.review_replies.lock().push_back(json!({"decision":"approve","reason":"Owner requested verification of the installed package manager"}));
    system
        .test_actor_input(
            "root",
            "Check the installed winget version".into(),
            vec![],
            None,
        )
        .unwrap();
    let message = system.store().claim_next("root").unwrap().unwrap();
    let actor = system.execution_actor(&message).unwrap();
    let context = system.execution_context(&actor, &message).unwrap();
    let args = json!({"command":"$ErrorActionPreference='Stop'; & winget --version; exit $LASTEXITCODE",
        "workdir":context.execution_cwd,"yield_time_ms":90000,"max_chars":2000,"session_id":null});
    let mut apps = AppManager::new(vec![
        Box::new(crate::sudo_app::SudoApp),
        Box::new(crate::terminal_app::TerminalApp::new()),
    ])
    .unwrap();
    let mut history = vec![];
    let control = ActorControl::default();
    execute_call(
        &system,
        &actor,
        &message,
        &control,
        &mut apps,
        &AgentToolCall {
            id: "native-winget".into(),
            name: "terminal__terminal_exec".into(),
            arguments: args.clone(),
        },
        &mut history,
    )
    .await
    .unwrap();
    let summary = system
        .store()
        .tool_summaries("root", &message.id, None, 10)
        .unwrap()
        .items
        .into_iter()
        .find(|tool| tool.call_id == "native-winget")
        .unwrap();
    let restricted = system.store().tool_result(&summary.id).unwrap();
    assert_ne!(
        restricted.payload["session"]["exit_code"],
        json!(0),
        "native container unexpectedly ran the packaged application"
    );
    let call = AgentToolCall {
        id: "sudo-winget".into(),
        name: "sudo__execute".into(),
        arguments: json!({
        "tool":"terminal__terminal_exec","arguments_json":args.to_string(),"reason":"Check the Owner-requested installed winget version",
        "restart_native":false,"approval_id":null}),
    };
    execute_call(
        &system,
        &actor,
        &message,
        &control,
        &mut apps,
        &call,
        &mut history,
    )
    .await
    .unwrap();
    let summary = system
        .store()
        .tool_summaries("root", &message.id, None, 10)
        .unwrap()
        .items
        .into_iter()
        .find(|tool| tool.call_id == "sudo-winget")
        .unwrap();
    let host = system.store().tool_result(&summary.id).unwrap();
    assert_eq!(
        host.payload["session"]["exit_code"],
        json!(0),
        "{}",
        host.model_content
    );
    assert_eq!(
        host.payload["output"].as_str().unwrap().trim(),
        expected.trim()
    );
    assert_eq!(host.payload["sudo"]["administrator"], json!(false));
    assert_eq!(provider.review_requests.lock().len(), 1);
    apps.shutdown().await.unwrap();
    system.shutdown().await;
}
