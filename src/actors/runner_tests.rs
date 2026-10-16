use super::super::{MessageKind, MessageSource, ModelFactory};
use super::*;
use crate::{
    context_budget::RequestBudgetLimits,
    core::TokenUsageInfo,
    model_protocol::{AgentContentPart, PromptRequest},
};
use std::{collections::VecDeque, sync::atomic::AtomicBool};

enum MockReply {
    Answer(&'static str),
    Calls(Vec<AgentToolCall>),
    Pending,
    FollowUp(&'static str),
    ReasoningOnly,
    Failure(&'static str),
}

async fn wait_passivated(system: &ActorSystem) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while system.resident_workers() != 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("Idle workers must release their App instances and registry entries");
}

#[tokio::test]
async fn idle_actors_activate_on_demand_and_reload_durable_history() {
    let provider = Arc::new(ReplayProvider::new(vec![
        MockReply::Answer("First"),
        MockReply::Answer("Second"),
    ]));
    let (_dir, mut system) = runtime(provider.clone()).await;
    system.set_idle_timeout(Duration::from_millis(20));
    let root = system.actor("root").unwrap();
    for _ in 0..64 {
        system.create_owned("root", root.config.clone()).unwrap();
    }
    assert_eq!(system.resident_workers(), 0);
    assert_eq!(system.store().actor_count().unwrap(), 65);
    let first = system
        .send_user("root", "Remember this request".into(), vec![], None)
        .unwrap();
    assert_eq!(
        wait_completed(&system, &first.id).await.status,
        MessageStatus::Completed
    );
    wait_passivated(&system).await;
    let context = system.actor("root").unwrap().context;
    assert_eq!(system.store().actor_count().unwrap(), 65);
    assert_eq!(
        system.store().message(&first.id).unwrap().reply.as_deref(),
        Some("First")
    );
    let second = system
        .send_user("root", "Continue".into(), vec![], None)
        .unwrap();
    assert_eq!(
        wait_completed(&system, &second.id).await.status,
        MessageStatus::Completed
    );
    wait_passivated(&system).await;
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
    let provider = Arc::new(ReplayProvider::new(
        (0..24).map(|_| MockReply::Answer("Done")).collect(),
    ));
    let (_dir, mut system) = runtime(provider.clone()).await;
    system.set_idle_timeout(Duration::from_millis(1));
    for round in 0..6 {
        let mut sends = tokio::task::JoinSet::new();
        for input in 0..4 {
            let system = system.clone();
            sends.spawn(async move {
                system
                    .send_user("root", format!("{round}/{input}"), vec![], None)
                    .unwrap()
            });
        }
        while let Some(sent) = sends.join_next().await {
            let message = sent.unwrap();
            assert_eq!(
                wait_completed(&system, &message.id).await.status,
                MessageStatus::Completed
            );
        }
        wait_passivated(&system).await;
    }
    assert_eq!(provider.requests.lock().len(), 24);
    system.shutdown().await;
}

#[tokio::test]
async fn reopening_starts_only_queued_actors_and_never_replays_completed_input() {
    let provider = Arc::new(ReplayProvider::new(vec![
        MockReply::Answer("Done"),
        MockReply::Answer("After restart"),
    ]));
    let (dir, mut system) = runtime(provider.clone()).await;
    system.set_idle_timeout(Duration::from_millis(20));
    let root = system.actor("root").unwrap();
    let child = system.create_owned("root", root.config.clone()).unwrap();
    let first = system
        .send_user("root", "First".into(), vec![], None)
        .unwrap();
    wait_completed(&system, &first.id).await;
    wait_passivated(&system).await;
    system.suspend_execution().await.unwrap();
    let queued = system
        .send_user(&child.id, "Queued".into(), vec![], None)
        .unwrap();
    assert_eq!(system.resident_workers(), 0);
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
        .send_user("root", "Finish the request".into(), vec![], None)
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
async fn empty_reasoning_has_a_fuse_and_transient_requests_can_reach_the_fifth_attempt() {
    let provider = Arc::new(ReplayProvider::new(vec![
        MockReply::ReasoningOnly,
        MockReply::ReasoningOnly,
        MockReply::ReasoningOnly,
    ]));
    let (_dir, system) = runtime(provider.clone()).await;
    let input = system
        .send_user("root", "Finish".into(), vec![], None)
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
    let (dir, system) = runtime(provider.clone()).await;
    let input = system
        .send_user("root", "Retry".into(), vec![], None)
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
    assert!(dir.path().exists());
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
    assert!(bounded.len() <= 1024 && bounded.contains("actors__read_resource"));
    let resource = crate::resources::export_all(&system, "root")
        .unwrap()
        .into_iter()
        .find(|resource| resource.name.starts_with("output-"))
        .unwrap();
    let input = system
        .send_user(
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
        crate::resources::bytes(&system, "root", &resource).unwrap(),
        original.as_bytes()
    );
    let input = system
        .send_user("root", "Next".into(), vec![], None)
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
    system.shutdown().await;
}

struct ReplayProvider {
    replies: parking_lot::Mutex<VecDeque<MockReply>>,
    requests: parking_lot::Mutex<Vec<AgentTurnRequest>>,
    started: tokio::sync::Notify,
    dropped: Arc<AtomicBool>,
    usage: parking_lot::Mutex<TokenUsageInfo>,
    usage_bias: std::sync::atomic::AtomicUsize,
}

impl ReplayProvider {
    fn new(replies: Vec<MockReply>) -> Self {
        Self {
            replies: parking_lot::Mutex::new(replies.into()),
            requests: parking_lot::Mutex::new(Vec::new()),
            started: tokio::sync::Notify::new(),
            dropped: Arc::new(AtomicBool::new(false)),
            usage: parking_lot::Mutex::new(TokenUsageInfo::default()),
            usage_bias: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

struct DropMarker(Arc<AtomicBool>);
impl Drop for DropMarker {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

#[async_trait]
impl ModelProvider for ReplayProvider {
    async fn complete_json(
        &self,
        _: PromptRequest,
        _: ModelRequestOptions,
    ) -> Result<serde_json::Value> {
        panic!("Actor runner must only use agent-turn protocol");
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
        self.requests.lock().push(request);
        self.started.notify_one();
        let reply = self
            .replies
            .lock()
            .pop_front()
            .expect("mock reply available");
        let bias = self.usage_bias.load(Ordering::Relaxed);
        if bias > 0 && !matches!(reply, MockReply::Pending) {
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
            MockReply::Failure(error) => Err(miette!("{error}")),
            MockReply::ReasoningOnly => Ok(AgentTurnStreamResult {
                items: vec![],
                raw_stream_follow_up: true,
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
                last_assistant_message: Some(text.into()),
                last_reasoning_content: None,
                last_reasoning_signature: None,
                responses_output: Some(vec![
                    json!({"type":"message","phase":"partial_answer","content":[{"type":"output_text","text":text}]}),
                ]),
            }),
            MockReply::Answer(text) => Ok(AgentTurnStreamResult {
                items: vec![],
                raw_stream_follow_up: false,
                last_assistant_message: Some(text.into()),
                last_reasoning_content: None,
                last_reasoning_signature: None,
                responses_output: Some(vec![
                    json!({"type":"message","content":[{"type":"output_text","text":text}]}),
                ]),
            }),
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
                    last_assistant_message: Some("Inspecting".into()),
                    last_reasoning_content: Some("private reasoning".into()),
                    last_reasoning_signature: Some("provider signature".into()),
                    responses_output: Some(output),
                })
            }
            MockReply::Pending => {
                let _marker = DropMarker(self.dropped.clone());
                std::future::pending().await
            }
        }
    }
    fn request_budget_limits(&self) -> RequestBudgetLimits {
        RequestBudgetLimits {
            context_window_tokens: 100_000,
            auto_compact_threshold_tokens: 100_000,
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

#[tokio::test]
#[ignore = "requires DAAT_LOCUS_TEST_STAGE_SIGNALING with a WSS rendezvous"]
async fn public_account_connection_syncs_messages_and_model_configuration() {
    use crate::stage::{connection::Route, host::Host, runtime::Runtime, shared};
    let endpoint = std::env::var("DAAT_LOCUS_TEST_STAGE_SIGNALING").unwrap();
    let (_a_dir, a) = runtime(Arc::new(ReplayProvider::new(vec![MockReply::Answer(
        "Public sync fixture reply",
    )])))
    .await;
    let (_b_dir, b) = runtime(Arc::new(ReplayProvider::new(vec![]))).await;
    let mut config = a.config();
    config.models.get_mut("mock").unwrap().context_window_tokens = 123_456;
    a.update_config(config).await.unwrap();
    let a_host = Host::load(a.clone()).unwrap();
    let b_host = Host::load(b.clone()).unwrap();
    a_host.initialize(true).unwrap();
    for host in [&a_host, &b_host] {
        let mut settings = host.settings();
        settings.signaling_origin = Some(endpoint.clone());
        settings.interfaces = vec!["fixture-no-lan-interface".into()];
        host.save_settings(settings).unwrap();
    }
    let a_runtime = Runtime::new(a_host.clone());
    a_runtime.start().await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while !a_runtime.core.read().as_ref().unwrap().permitted() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    let input = a
        .send_user(
            shared::ACTOR_ID,
            "Message before public sync".into(),
            vec![],
            None,
        )
        .unwrap();
    let result = wait_completed(&a, &input.id).await;
    assert_eq!(result.reply.as_deref(), Some("Public sync fixture reply"));
    let join = b_host.initialize(false).unwrap();
    let own = a_host.enrollment().unwrap();
    b_host
        .join(
            a_host.enroll_authenticated_device(&join).unwrap(),
            &own.root.group_id,
        )
        .unwrap();
    let b_runtime = Runtime::new(b_host.clone());
    b_runtime.start().await.unwrap();
    let synced = tokio::time::timeout(Duration::from_secs(45), async {
        loop {
            if b.store()
                .message(&input.id)
                .is_ok_and(|message| message.reply.as_deref() == Some("Public sync fixture reply"))
                && b.config()
                    .models
                    .get("mock")
                    .is_some_and(|model| model.context_window_tokens == 123_456)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
    let a_state = a_runtime.state(b_host.local);
    let b_state = b_runtime.state(a_host.local);
    a_runtime.stop().await;
    b_runtime.stop().await;
    a.shutdown().await;
    b.shutdown().await;
    assert!(
        synced.is_ok() && a_state.route == Some(Route::P2p) && b_state.route == Some(Route::P2p),
        "messages and configuration did not synchronize automatically over public P2P: {a_state:?}, {b_state:?}"
    );
}
impl ModelFactory for ReplayFactory {
    fn build(&self, _: &str, _: &crate::config::Config) -> Result<Arc<dyn ModelProvider>> {
        Ok(self.0.clone())
    }
}

#[tokio::test]
async fn single_device_account_actor_runs_without_cloud_services() {
    use crate::stage::{host::Host, runtime::Runtime, shared};
    let model = Arc::new(ReplayProvider::new(vec![MockReply::Answer(
        "Local account execution completed.",
    )]));
    let (_directory, system) = runtime(model.clone()).await;
    let host = Host::load(system.clone()).unwrap();
    host.initialize(true).unwrap();
    let network = Runtime::new(host);
    network.start().await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while !network.core.read().as_ref().unwrap().permitted() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    assert!(network.address.read().is_some());
    assert!(network.signals.read().is_none());
    let input = system
        .send_user(shared::ACTOR_ID, "Work locally.".into(), vec![], None)
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while system.store().message(&input.id).unwrap().status != MessageStatus::Completed {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        system.store().message(&input.id).unwrap().reply.as_deref(),
        Some("Local account execution completed.")
    );
    assert_eq!(model.requests.lock().len(), 1);
    network.stop().await;
    system.shutdown().await;
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
) {
    use crate::stage::{host::Host, ledger::StageLedger, runtime::Runtime};
    if std::env::var("DAAT_LOCUS_TEST_STAGE_TRACE").as_deref() == Ok("1") {
        let _ = tracing_subscriber::fmt()
            .with_env_filter("daat_locus::stage=debug,openraft=warn")
            .with_test_writer()
            .try_init();
    }
    let (a_dir, a) = runtime(model_a).await;
    let (b_dir, b) = runtime(model_b).await;
    let host_a = Host::load(a.clone()).unwrap();
    host_a.initialize(true).unwrap();
    let host_b = Host::load(b.clone()).unwrap();
    let request = host_b.initialize(false).unwrap();
    host_b
        .join(
            fixture_enrollment(&host_a, &request, true, vec![]).unwrap(),
            &host_a.account_id().unwrap(),
        )
        .unwrap();
    StageLedger::attach(b.store(), host_b.local)
        .import_peer(
            host_a.enrollment().unwrap(),
            vec![],
            chrono::Utc::now().timestamp_millis(),
        )
        .unwrap();
    for (host, priority) in [(&host_a, 100), (&host_b, 200)] {
        let mut settings = host.settings();
        settings.listen = "127.0.0.1:0".into();
        settings.compute_priority = priority;
        host.save_settings(settings).unwrap();
    }
    let net_a = Runtime::new(host_a);
    let net_b = Runtime::new(host_b);
    net_a.start().await.unwrap();
    net_b.start().await.unwrap();
    let address = net_a.address.read().unwrap();
    net_b
        .dial_tcp(net_a.host.local, address, None)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        while !net_a.core.read().as_ref().unwrap().permitted()
            || !net_b.core.read().as_ref().unwrap().accepts_input()
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("device pair did not synchronize");
    ((a_dir, a, net_a), (b_dir, b, net_b))
}

#[tokio::test]
async fn an_unresponsive_learner_does_not_block_the_existing_worker_renewal_or_inbox() {
    use crate::stage::shared;
    let model_a = Arc::new(ReplayProvider::new(vec![MockReply::Answer(
        "The worker remained usable.",
    )]));
    let model_b = Arc::new(ReplayProvider::new(vec![]));
    let ((_a_dir, a, net_a), (_b_dir, b, net_b)) = device_pair(model_a.clone(), model_b).await;
    let coordinator = net_b.coordination.read().clone().unwrap();
    coordinator.stop().await;
    let core = net_a.core.read().clone().unwrap();
    for _ in 0..35 {
        assert!(
            core.permitted(),
            "a lagging learner interrupted a healthy worker"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let input = a
        .send_user(
            shared::ACTOR_ID,
            "Work while the replica cannot acknowledge.".into(),
            vec![],
            None,
        )
        .unwrap();
    assert_eq!(
        wait_completed(&a, &input.id).await.reply.as_deref(),
        Some("The worker remained usable.")
    );
    assert_eq!(model_a.requests.lock().len(), 1);
    net_b.stop_abruptly().await;
    net_a.stop_abruptly().await;
    a.shutdown().await;
    b.shutdown().await;
}
#[tokio::test]
async fn two_device_graceful_exit_transfers_authority_and_keeps_the_remaining_device_usable() {
    use crate::stage::{shared, shared_store};
    let model_a = Arc::new(ReplayProvider::new(vec![]));
    let model_b = Arc::new(ReplayProvider::new(vec![MockReply::Answer(
        "Continued after the other device exited.",
    )]));
    let ((_a_dir, a, net_a), (_b_dir, b, net_b)) = device_pair(model_a, model_b.clone()).await;
    assert_eq!(
        net_b.voting_member(net_b.host.local),
        Some(false),
        "joining must not add a voting dependency"
    );
    net_a.stop().await;
    tokio::time::timeout(Duration::from_secs(15), async {
        while !net_b.core.read().as_ref().unwrap().permitted() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("remaining device did not become ready");
    assert_eq!(
        net_b.voting_member(net_b.host.local),
        Some(true),
        "graceful handoff must commit real membership"
    );
    assert!(
        crate::stage::recovery::branch(b.store()).unwrap().is_none(),
        "normal handoff must not need an isolated branch"
    );
    let input = b
        .send_user(shared::ACTOR_ID, "Continue here.".into(), vec![], None)
        .unwrap();
    assert_eq!(
        wait_completed(&b, &input.id).await.reply.as_deref(),
        Some("Continued after the other device exited.")
    );
    assert!(shared_store::head(b.store()).unwrap().sequence > 0);
    assert_eq!(model_b.requests.lock().len(), 1);
    net_b.stop().await;
    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test]
async fn two_device_abrupt_loss_runs_locally_and_reconnects_without_repeating_completed_work() {
    use crate::stage::{shared, shared_store};
    let model_a = Arc::new(ReplayProvider::new(vec![MockReply::Answer(
        "Before disconnection.",
    )]));
    let model_b = Arc::new(ReplayProvider::new(vec![MockReply::Answer(
        "Completed while disconnected.",
    )]));
    let ((_a_dir, a, net_a), (_b_dir, b, net_b)) =
        device_pair(model_a.clone(), model_b.clone()).await;
    let first = a
        .send_user(
            shared::ACTOR_ID,
            "Remember this conversation.".into(),
            vec![],
            None,
        )
        .unwrap();
    wait_completed(&a, &first.id).await;
    tokio::time::timeout(Duration::from_secs(10), async {
        while shared_store::head(a.store()).unwrap() != shared_store::head(b.store()).unwrap() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    net_a.stop_abruptly().await;
    tokio::time::timeout(Duration::from_secs(15), async {
        while !net_b.core.read().as_ref().unwrap().permitted() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("local recovery did not start");
    assert_eq!(
        net_b.core.read().as_ref().unwrap().status()["state"],
        "local_recovery"
    );
    assert_eq!(
        net_b.voting_member(net_b.host.local),
        Some(false),
        "local recovery must not invent a Raft voter"
    );
    let second = b
        .send_user(
            shared::ACTOR_ID,
            "Work without the other device.".into(),
            vec![],
            None,
        )
        .unwrap();
    assert_eq!(
        wait_completed(&b, &second.id).await.reply.as_deref(),
        Some("Completed while disconnected.")
    );
    net_a.start().await.unwrap();
    let address = net_a.address.read().unwrap();
    net_b
        .dial_tcp(net_a.host.local, address, None)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(30), async {
        while shared_store::head(a.store()).unwrap() != shared_store::head(b.store()).unwrap()
            || crate::stage::recovery::branch(b.store()).unwrap().is_some()
            || !net_a.core.read().as_ref().unwrap().accepts_input()
            || !net_b.core.read().as_ref().unwrap().accepts_input()
        {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("recovered branch did not rejoin");
    assert_eq!(
        a.store().message(&second.id).unwrap().reply.as_deref(),
        Some("Completed while disconnected.")
    );
    assert_eq!(model_a.requests.lock().len(), 1);
    assert_eq!(model_b.requests.lock().len(), 1);
    net_b.stop().await;
    net_a.stop().await;
    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test]
async fn connected_learners_continue_after_authority_loss_and_rejoin_without_replaying_work() {
    use crate::stage::{host::Host, recovery, runtime::Runtime, shared, shared_store};
    let model_a = Arc::new(ReplayProvider::new(vec![]));
    let model_b = Arc::new(ReplayProvider::new(vec![MockReply::Answer(
        "The first surviving replica completed its request.",
    )]));
    let model_c = Arc::new(ReplayProvider::new(vec![MockReply::Answer(
        "The second surviving replica completed its request.",
    )]));
    let ((_a_dir, a, net_a), (_b_dir, b, net_b)) =
        device_pair(model_a.clone(), model_b.clone()).await;
    let (_c_dir, c) = runtime(model_c.clone()).await;
    let host_c = Host::load(c.clone()).unwrap();
    let enrollment_request = host_c.initialize(false).unwrap();
    host_c
        .join(
            fixture_enrollment(&net_a.host, &enrollment_request, true, vec![]).unwrap(),
            &net_a.host.account_id().unwrap(),
        )
        .unwrap();
    host_c
        .discover_member(net_a.host.enrollment().unwrap())
        .unwrap();
    host_c
        .discover_member(net_b.host.enrollment().unwrap())
        .unwrap();
    for host in [&net_a.host, &net_b.host] {
        host.discover_member(host_c.enrollment().unwrap()).unwrap();
    }
    let mut settings = host_c.settings();
    settings.listen = "127.0.0.1:0".into();
    settings.compute_priority = 300;
    host_c.save_settings(settings).unwrap();
    let net_c = Runtime::new(host_c);
    net_c.start().await.unwrap();
    let address_a = net_a.address.read().unwrap();
    let address_b = net_b.address.read().unwrap();
    net_c
        .dial_tcp(net_a.host.local, address_a, None)
        .await
        .unwrap();
    net_c
        .dial_tcp(net_b.host.local, address_b, None)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        while !net_c.core.read().as_ref().unwrap().accepts_input()
            || !net_b.connected(net_c.host.local)
            || !recovery::synchronized(c.store()).unwrap()
        {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("third replica did not synchronize");
    net_a.stop_abruptly().await;
    tokio::time::timeout(Duration::from_secs(15), async {
        while !net_b.core.read().as_ref().unwrap().permitted()
            || !net_c.core.read().as_ref().unwrap().permitted()
        {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("connected learners waited indefinitely for the lost authority");
    assert!(net_b.connected(net_c.host.local));
    assert!(net_c.connected(net_b.host.local));
    for network in [&net_b, &net_c] {
        assert_eq!(network.voting_member(network.host.local), Some(false));
        assert_eq!(
            network.core.read().as_ref().unwrap().status()["state"],
            "local_recovery"
        );
    }
    for (system, reply) in [
        (&b, "The first surviving replica completed its request."),
        (&c, "The second surviving replica completed its request."),
    ] {
        let input = system
            .send_user(shared::ACTOR_ID, "Continue here.".into(), vec![], None)
            .unwrap();
        assert_eq!(
            wait_completed(system, &input.id).await.reply.as_deref(),
            Some(reply)
        );
    }
    net_a.start().await.unwrap();
    let address_a = net_a.address.read().unwrap();
    for network in [&net_b, &net_c] {
        network
            .dial_tcp(net_a.host.local, address_a, None)
            .await
            .unwrap();
    }
    let reconciled = tokio::time::timeout(Duration::from_secs(30), async {
        while shared_store::head(a.store()).unwrap() != shared_store::head(b.store()).unwrap()
            || shared_store::head(a.store()).unwrap() != shared_store::head(c.store()).unwrap()
            || recovery::branch(b.store()).unwrap().is_some()
            || recovery::branch(c.store()).unwrap().is_some()
            || !net_b.core.read().as_ref().unwrap().accepts_input()
            || !net_c.core.read().as_ref().unwrap().accepts_input()
        {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    assert!(
        reconciled.is_ok(),
        "surviving replica branches did not reconcile: a={} b={} c={}, heads={:?}, branches={:?}",
        net_a.core.read().as_ref().unwrap().status(),
        net_b.core.read().as_ref().unwrap().status(),
        net_c.core.read().as_ref().unwrap().status(),
        [&a, &b, &c].map(|system| shared_store::head(system.store()).unwrap()),
        [&a, &b, &c].map(|system| recovery::branch(system.store()).unwrap())
    );
    for system in [&a, &b, &c] {
        let replies = system
            .store()
            .messages(shared::ACTOR_ID, None, 100)
            .unwrap()
            .items
            .into_iter()
            .filter_map(|message| message.reply)
            .collect::<std::collections::HashSet<_>>();
        assert!(replies.contains("The first surviving replica completed its request."));
        assert!(replies.contains("The second surviving replica completed its request."));
        assert!(!recovery::archives_pending(system.store()).unwrap());
    }
    assert!(model_a.requests.lock().is_empty());
    assert_eq!(model_b.requests.lock().len(), 1);
    assert_eq!(model_c.requests.lock().len(), 1);
    net_c.stop_abruptly().await;
    net_b.stop_abruptly().await;
    net_a.stop_abruptly().await;
    for system in [&a, &b, &c] {
        system.shutdown().await;
    }
}

#[tokio::test]
async fn two_device_partition_preserves_both_branches_and_does_not_replay_the_losing_branch() {
    use crate::stage::{recovery, shared, shared_store};
    let model_a = Arc::new(ReplayProvider::new(vec![MockReply::Answer(
        "The first offline outcome.",
    )]));
    let model_b = Arc::new(ReplayProvider::new(vec![MockReply::Answer(
        "The second offline outcome.",
    )]));
    let ((_a_dir, a, net_a), (_b_dir, b, net_b)) =
        device_pair(model_a.clone(), model_b.clone()).await;
    net_a.disconnect_peers();
    net_b.disconnect_peers();
    tokio::time::timeout(Duration::from_secs(15), async {
        while !net_b.core.read().as_ref().unwrap().permitted() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    let left = a
        .send_user(
            shared::ACTOR_ID,
            "First offline request.".into(),
            vec![],
            None,
        )
        .unwrap();
    let right = b
        .send_user(
            shared::ACTOR_ID,
            "Second offline request.".into(),
            vec![],
            None,
        )
        .unwrap();
    wait_completed(&a, &left.id).await;
    wait_completed(&b, &right.id).await;
    assert_ne!(
        shared_store::head(a.store()).unwrap(),
        shared_store::head(b.store()).unwrap()
    );
    let address = net_a.address.read().unwrap();
    net_b
        .dial_tcp(net_a.host.local, address, None)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(30), async {
        while shared_store::head(a.store()).unwrap() != shared_store::head(b.store()).unwrap()
            || recovery::branch(b.store()).unwrap().is_some()
            || !net_a.core.read().as_ref().unwrap().accepts_input()
            || !net_b.core.read().as_ref().unwrap().accepts_input()
        {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("partition branches did not reconcile");
    for system in [&a, &b] {
        let replies = system
            .store()
            .messages(shared::ACTOR_ID, None, 100)
            .unwrap()
            .items
            .into_iter()
            .filter_map(|message| message.reply)
            .collect::<std::collections::HashSet<_>>();
        assert!(replies.contains("The first offline outcome."));
        assert!(replies.contains("The second offline outcome."));
        assert!(!recovery::archives_pending(system.store()).unwrap());
    }
    assert_eq!(model_a.requests.lock().len(), 1);
    assert_eq!(model_b.requests.lock().len(), 1);
    assert!(
        recovery::archive_count(a.store()).unwrap() + recovery::archive_count(b.store()).unwrap()
            > 0
    );
    net_b.stop().await;
    net_a.stop().await;
    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test]
async fn three_device_core_preserves_resources_context_and_effects_across_sequential_takeover() {
    if std::env::var("DAAT_LOCUS_TEST_STAGE_TRACE").as_deref() == Ok("1") {
        let _ = tracing_subscriber::fmt()
            .with_env_filter("daat_locus::stage=debug,openraft=info")
            .with_test_writer()
            .try_init();
    }
    use crate::stage::{host::Host, ledger::StageLedger, runtime::Runtime, shared, shared_store};
    let model_a = Arc::new(ReplayProvider::new(vec![]));
    let (_a_dir, a) = runtime(model_a.clone()).await;
    let model_b = Arc::new(ReplayProvider::new(vec![MockReply::Answer(
        "Continued on the next device without repeating Actor creation.",
    )]));
    let (_b_dir, b) = runtime(model_b.clone()).await;
    let model_c = Arc::new(ReplayProvider::new(vec![MockReply::Answer(
        "Continued on the third device with the same account context.",
    )]));
    let (_c_dir, c) = runtime(model_c.clone()).await;
    for system in [&a, &b, &c] {
        let mut config = system.config();
        config.models.get_mut("mock").unwrap().supports_vision = Some(true);
        system.update_config(config).await.unwrap();
    }
    let host_c = Host::load(c.clone()).unwrap();
    let host_a = Host::load(a.clone()).unwrap();
    let host_b = Host::load(b.clone()).unwrap();
    host_a.initialize(true).unwrap();
    let request_b = host_b.initialize(false).unwrap();
    let enrollment = fixture_enrollment(&host_a, &request_b, true, vec![]).unwrap();
    host_b
        .join(enrollment, &host_a.account_id().unwrap())
        .unwrap();
    StageLedger::attach(b.store(), host_b.local)
        .import_peer(
            host_a.enrollment().unwrap(),
            vec![],
            chrono::Utc::now().timestamp_millis(),
        )
        .unwrap();
    let request_c = host_c.initialize(false).unwrap();
    host_c
        .join(
            fixture_enrollment(&host_a, &request_c, true, vec![]).unwrap(),
            &host_a.account_id().unwrap(),
        )
        .unwrap();
    host_b
        .discover_member(host_c.enrollment().unwrap())
        .unwrap();
    host_c
        .discover_member(host_b.enrollment().unwrap())
        .unwrap();
    for (host, priority) in [(&host_a, 100), (&host_b, 200), (&host_c, 300)] {
        let mut settings = host.settings();

        settings.compute_priority = priority;
        settings.listen = "127.0.0.1:0".into();
        host.save_settings(settings).unwrap();
    }
    let net_a = Runtime::new(host_a.clone());
    let net_b = Runtime::new(host_b.clone());
    let net_c = Runtime::new(host_c.clone());
    net_a.start().await.unwrap();
    net_b.start().await.unwrap();
    net_c.start().await.unwrap();
    let address = net_b.address.read().unwrap();
    net_a.dial_tcp(host_b.local, address, None).await.unwrap();
    let address_c = net_c.address.read().unwrap();
    net_a.dial_tcp(host_c.local, address_c, None).await.unwrap();
    net_b.dial_tcp(host_c.local, address_c, None).await.unwrap();
    for _ in 0..100 {
        if net_a
            .core
            .read()
            .as_ref()
            .is_some_and(|core| core.permitted())
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        net_a.core.read().as_ref().unwrap().permitted(),
        "initial worker was not acquired: a={} b={} c={}",
        net_a.core.read().as_ref().unwrap().status(),
        net_b.core.read().as_ref().unwrap().status(),
        net_c.core.read().as_ref().unwrap().status()
    );
    let mut settings = host_b.settings();
    settings.compute_priority = 0;
    host_b.save_settings(settings).unwrap();
    tokio::time::sleep(Duration::from_millis(2200)).await;
    assert!(
        net_a.core.read().as_ref().unwrap().permitted(),
        "a healthy worker must retain affinity"
    );
    assert!(
        !net_b.core.read().as_ref().unwrap().permitted(),
        "higher priority must not preempt a healthy worker"
    );
    let constraints = a.actor(shared::ACTOR_ID).unwrap().config;
    b.patch_owned_preferences(
        shared::ACTOR_ID,
        None,
        Some(super::super::ApprovalMode::None),
    )
    .await
    .unwrap();
    let saved = a.actor(shared::ACTOR_ID).unwrap().config;
    let mut expected = constraints;
    expected.permissions.approval_mode = super::super::ApprovalMode::None;
    assert_eq!(
        saved, expected,
        "shared mode preference must not rewrite creation constraints"
    );
    model_a.replies.lock().extend([
        MockReply::Calls(vec![AgentToolCall {
            id: "one-device-effect".into(), name: "actors__create".into(),
            arguments: json!({"stage_id":host_b.local,"name":"Hosted project","workspace":null,"read_only":false,"apps":["files"],"instructions":null,"model":null}),
        }]),
        MockReply::Calls(vec![AgentToolCall {
            id: "remote-image".into(), name: "actors__read_resource".into(),
            arguments: json!({"resource_id":"remote-image","offset":null,"limit":1024}),
        }]),
        MockReply::Pending, MockReply::Pending,
    ]);
    model_a.usage_bias.store(37, Ordering::Relaxed);
    let image = b"\x89PNG\r\n\x1a\nremote view image";
    let image_path = b.actor_data_dir(shared::ACTOR_ID).join("inspect.png");
    std::fs::write(&image_path, image).unwrap();
    b.store()
        .register_resource(super::super::StoredResource {
            id: "remote-image".into(),
            actor_id: shared::ACTOR_ID.into(),
            path: image_path,
            name: "inspect.png".into(),
            media_type: "image/png".into(),
            size_bytes: image.len() as u64,
            uri: "/v1/actors/core/resources/remote-image".into(),
        })
        .unwrap();
    let attachment_path = b.actor_data_dir(shared::ACTOR_ID).join("fixture.txt");
    std::fs::write(&attachment_path, b"cross device attachment").unwrap();
    let attachment = super::super::Attachment {
        id: "cross-device".into(),
        name: "fixture.txt".into(),
        media_type: "text/plain".into(),
        size_bytes: 23,
        uri: "/v1/actors/core/resources/cross-device".into(),
    };
    b.store()
        .register_resource(super::super::StoredResource {
            id: attachment.id.clone(),
            actor_id: shared::ACTOR_ID.into(),
            path: attachment_path,
            name: attachment.name.clone(),
            media_type: attachment.media_type.clone(),
            size_bytes: attachment.size_bytes,
            uri: attachment.uri.clone(),
        })
        .unwrap();
    let first_started = std::time::Instant::now();
    // User input from a non-worker device joins the same serial inbox.
    let input = b
        .send_user(
            shared::ACTOR_ID,
            "Create a hosted project on B, then continue if this worker disconnects.".into(),
            vec![attachment.clone()],
            Some("shared-user-input".into()),
        )
        .unwrap();
    for _ in 0..150 {
        if model_a.requests.lock().len() >= 3 {
            break;
        }
        if let Ok(state) = a.store().message(&input.id)
            && state.status != MessageStatus::Running
        {
            assert!(
                state.status == MessageStatus::Queued,
                "before transfer: {:?} {:?}",
                state.status,
                state.error
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        b.store()
            .actors()
            .unwrap()
            .iter()
            .any(|actor| actor.config.name == "Hosted project"),
        "no effect: A {:?}, B {:?}, C {:?}, requests A {}, errors AB {:?} AC {:?} BC {:?}, message A {:?}, message B {:?}",
        net_a.core.read().as_ref().unwrap().status(),
        net_b.core.read().as_ref().unwrap().status(),
        net_c.core.read().as_ref().unwrap().status(),
        model_a.requests.lock().len(),
        net_a.last_error(host_b.local),
        net_a.last_error(host_c.local),
        net_b.last_error(host_c.local),
        a.store()
            .message(&input.id)
            .map(|message| (message.status, message.error)),
        b.store()
            .message(&input.id)
            .map(|message| (message.status, message.error))
    );
    let hosted = b
        .store()
        .actors()
        .unwrap()
        .into_iter()
        .find(|actor| actor.config.name == "Hosted project")
        .unwrap();
    let address = crate::stage::ActorAddress {
        stage_id: host_b.local,
        actor_id: hosted.id.clone(),
    };
    let detail = crate::stage::observation::read(
        &b,
        host_c.local,
        &address,
        &crate::stage::observation::Query::Actor,
        chrono::Utc::now().timestamp_millis(),
    )
    .unwrap();
    assert_eq!(
        detail["access"]["manage"], true,
        "a third account device can access the actual dispatched Actor"
    );
    assert_eq!(detail["conversation_url"], address.conversation_url());
    let outcomes = a
        .store()
        .tool_summaries(shared::ACTOR_ID, &input.id, None, 30)
        .unwrap();
    let created = outcomes
        .items
        .iter()
        .find(|tool| tool.name == "actors__create")
        .unwrap();
    assert_eq!(
        created.outcome.as_ref().unwrap()["conversation_url"],
        address.conversation_url()
    );
    assert_eq!(
        b.store()
            .actors()
            .unwrap()
            .iter()
            .filter(|actor| actor.config.name == "Hosted project")
            .count(),
        1
    );
    assert!(
        !a.store()
            .actors()
            .unwrap()
            .iter()
            .any(|actor| actor.config.name == "Hosted project")
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        while shared_store::head(a.store()).unwrap() != shared_store::head(b.store()).unwrap() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    let guidance = b
        .send_user(
            shared::ACTOR_ID,
            "Keep the original request and answer in Chinese.".into(),
            vec![],
            None,
        )
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while a.store().message(&guidance.id).is_err() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    // Forwarding assigns the authoritative revision. Steering must use the
    // received current record rather than this device's initial enqueue time.
    let guidance_revision = a.store().message(&guidance.id).unwrap().updated_at_ms;
    b.steer_queued(shared::ACTOR_ID, &guidance.id, &input.id, guidance_revision)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while model_a.requests.lock().len() < 3 {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    let first_effect_ms = first_started.elapsed().as_millis();
    let a_b = net_a.handle(host_b.local).unwrap().traffic();
    let a_c = net_a.handle(host_c.local).unwrap().traffic();
    let takeover_b_started = std::time::Instant::now();
    net_a.stop().await;
    let result = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let input = b.store().message(&input.id).unwrap();
            if input.status == MessageStatus::Completed {
                break input;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        result.reply.as_deref(),
        Some("Continued on the next device without repeating Actor creation.")
    );
    let takeover_b_ms = takeover_b_started.elapsed().as_millis();
    assert!(model_a.dropped.load(Ordering::Acquire));
    assert_eq!(model_b.requests.lock().len(), 1);
    for request in model_b.requests.lock().iter() {
        let paths: Vec<_> = request
            .messages
            .iter()
            .filter_map(|row| {
                if let AgentMessage::User { content } = row {
                    Some(content.parts())
                } else {
                    None
                }
            })
            .flatten()
            .filter_map(|part| {
                if let AgentContentPart::ImageData { data_base64, .. } = part {
                    Some(data_base64)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(paths.len(), 1);
        use base64::Engine;
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(paths[0])
                .unwrap(),
            image
        );
    }
    assert!(
        b.actor(shared::ACTOR_ID)
            .unwrap()
            .context
            .usage
            .total_token_usage
            .input_tokens
            > 0
    );
    assert!(model_b.requests.lock()[0].messages.iter().any(|message|matches!(message,AgentMessage::User{content} if content.as_text().contains("answer in Chinese"))));
    assert!(model_b.requests.lock()[0].messages.iter().any(|message|matches!(message,AgentMessage::Tool{tool_call_id,..} if tool_call_id=="one-device-effect")));
    assert_eq!(
        b.store()
            .actors()
            .unwrap()
            .iter()
            .filter(|actor| actor.config.name == "Hosted project")
            .count(),
        1
    );
    tokio::time::timeout(Duration::from_secs(12), async {
        loop {
            if c.store()
                .message(&input.id)
                .is_ok_and(|m| m.status == MessageStatus::Completed)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap();
    let received = c
        .store()
        .resource(shared::ACTOR_ID, &attachment.id)
        .unwrap();
    assert_eq!(
        std::fs::read(received.path).unwrap(),
        b"cross device attachment"
    );
    model_b.replies.lock().push_back(MockReply::Pending);
    let next = c
        .send_user(
            shared::ACTOR_ID,
            "Continue after the second worker goes offline.".into(),
            vec![],
            None,
        )
        .unwrap();
    tokio::time::timeout(Duration::from_secs(12), async {
        while model_b.requests.lock().len() < 2 {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap();
    let b_c = net_b.handle(host_c.local).unwrap().traffic();
    let takeover_c_started = std::time::Instant::now();
    net_b.stop().await;
    tokio::time::timeout(Duration::from_secs(20), async {
        while c.store().message(&next.id).unwrap().status != MessageStatus::Completed {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect(
        "the last synchronized device must continue without waiting for another device to return",
    );
    assert!(net_c.core.read().as_ref().unwrap().permitted());
    assert_eq!(model_c.requests.lock().len(), 1);
    let mut returning = host_a.settings();
    returning.compute_priority = 999;
    host_a.save_settings(returning).unwrap();
    net_a.start().await.unwrap();
    let address_c = net_c.address.read().unwrap();
    let connected = net_a.dial_tcp(host_c.local, address_c, None).await;
    if connected.is_err() {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !net_a.connected(host_c.local) {
                let _ = net_a.dial_tcp(host_c.local, address_c, None).await;
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .expect("returned device must re-establish its authenticated channel");
    }

    // A returning replica must learn the completed outcome without requesting
    // another model turn or repeating the original device effect.
    tokio::time::timeout(Duration::from_secs(30), async {
        while a.store().message(&next.id).is_err()
            || shared_store::head(a.store()).unwrap() != shared_store::head(c.store()).unwrap()
        {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap_or_else(|error| {
        panic!(
            "returning replica must synchronize: {error:?}; A {}, C {}",
            net_a.core.read().as_ref().unwrap().status(),
            net_c.core.read().as_ref().unwrap().status()
        )
    });
    assert_eq!(model_c.requests.lock().len(), 1);
    assert!(model_c.requests.lock()[0].messages.iter().any(|row| matches!(row, AgentMessage::User { content } if content.parts().iter().any(|part| matches!(part, AgentContentPart::ImageData { data_base64, .. } if !data_base64.is_empty())))));
    assert!(model_c.requests.lock()[0].messages.iter().any(
        |m| matches!(m,AgentMessage::Tool{tool_call_id,..} if tool_call_id=="one-device-effect")
    ));
    assert_eq!(
        c.store().message(&next.id).unwrap().reply.as_deref(),
        Some("Continued on the third device with the same account context.")
    );
    let effects=b.store().stage_transaction(|tx|Ok(tx.query_row("SELECT COUNT(*) FROM core_app_effects WHERE state='completed' AND call='one-device-effect'",[],|r|r.get::<_,i64>(0))?)).unwrap();
    assert_eq!(effects, 1);
    std::fs::create_dir_all("target").unwrap();
    std::fs::write("target/multi-device-harness.json",serde_json::to_vec_pretty(&json!({"transport":"loopback mutual TLS; device-owned quorum; no cloud service","measurement":"encoded protocol frames, both directions; excludes IP/TLS overhead","first_effect_ms":first_effect_ms,"takeover_b_ms":takeover_b_ms,"takeover_c_ms":takeover_c_started.elapsed().as_millis(),"device_effect_count":effects,"channels":{"a_b":a_b,"a_c":a_c,"b_c":b_c}})).unwrap()).unwrap();
    net_c.stop().await;
    net_a.stop().await;
    a.shutdown().await;
    b.shutdown().await;
    c.shutdown().await;
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
    let directory = tempfile::tempdir().unwrap();
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
    let mut actor = system.store().actor("root").unwrap();
    actor.config.permissions.approval_mode = super::super::ApprovalMode::None;
    system.store().save_actor(&actor).unwrap();
    (directory, system)
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
            "Actor request did not finish: {message:?}; actor={:?}; core={:?}",
            system.actor(&message.actor_id).map(|a| a.status),
            system.shared_core().map(|c| c.status())
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn native_stage_tcp_routes_durable_user_work_and_resumes_a_shared_actor_view() {
    use crate::stage::{
        ActorAddress,
        host::Host,
        ledger::{ActorGrant, StageLedger},
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
    let grants = vec![ActorGrant {
        actor_id: "root".into(),
        read: true,
        converse: true,
        delegate: true,
        read_only: true,
    }];
    let enrollment_b = fixture_enrollment(&host_a, &request_b, false, grants.clone()).unwrap();
    host_b
        .join(enrollment_b, &host_a.account_id().unwrap())
        .unwrap();
    StageLedger::attach(b.store(), host_b.local)
        .import_peer(
            host_a.enrollment().unwrap(),
            grants,
            chrono::Utc::now().timestamp_millis(),
        )
        .unwrap();
    for host in [&host_a, &host_b] {
        let mut settings = host.settings();

        settings.listen = "127.0.0.1:0".into();
        host.save_settings(settings).unwrap();
    }
    let net_a = Runtime::new(host_a.clone());
    let net_b = Runtime::new(host_b.clone());
    net_a.start().await.unwrap();
    net_b.start().await.unwrap();
    let b_address = net_b.address.read().unwrap();
    net_a.dial_tcp(host_b.local, b_address, None).await.unwrap();
    let target = ActorAddress {
        stage_id: host_b.local,
        actor_id: "root".into(),
    };
    let view = uuid::Uuid::new_v4();
    let handle = net_a.handle(host_b.local).unwrap();
    handle
        .watch("root", view, SubscriptionLevel::Detail)
        .unwrap();
    let frame = a
        .send_stage_user(
            target.clone(),
            "Respond over the native peer channel.".into(),
            "tcp-client-operation",
        )
        .unwrap();
    assert_eq!(
        a.send_stage_user(
            target.clone(),
            "Respond over the native peer channel.".into(),
            "tcp-client-operation"
        )
        .unwrap()
        .invocation_id,
        frame.invocation_id
    );
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
    assert!(matches!(directory,Body::Directory{actors,..}if actors.len()==1));
    handle.unwatch("root", view);
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
        ledger::{ActorGrant, StageLedger},
        protocol::{BlobAccess, Body},
        receiver::StageReceiver,
        transport::tls,
    };
    let peer_model = Arc::new(ReplayProvider::new(vec![
        MockReply::Calls(vec![AgentToolCall {
            id: "remote-write-once".into(),
            name: "files__write_file".into(),
            arguments: json!({"path":"one.txt","content":"one effect","expected_sha256":null}),
        }]),
        MockReply::Calls(vec![AgentToolCall {
            id: "remote-progress".into(),
            name: "actors__report".into(),
            arguments: json!({"content":"The file is written; checking the result."}),
        }]),
        MockReply::Answer("Remote work is complete."),
    ]));
    let (_peer_dir, b) = runtime(peer_model.clone()).await;
    let b_id = b.store().public_stage_id().unwrap();
    let caller_model = Arc::new(ReplayProvider::new(vec![
        MockReply::Calls(vec![AgentToolCall {
            id: "dispatch-once".into(),
            name: "actors__send".into(),
            arguments: json!({"actor_id":"root","stage_id":b_id.to_string(),"content":"Write one file and report."}),
        }]),
        MockReply::Answer("The task is delegated."),
        MockReply::Answer("The collaborator reported progress."),
        MockReply::Answer("The delegated result arrived."),
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
    ledger_a.import_peer(member_b, vec![], now).unwrap();
    ledger_b
        .import_peer(
            own,
            vec![ActorGrant {
                actor_id: "root".into(),
                read: true,
                converse: true,
                delegate: true,
                read_only: false,
            }],
            now,
        )
        .unwrap();
    let nonce = uuid::Uuid::new_v4();
    ledger_a.commit_generation(b_id, 0, 1, nonce, now).unwrap();
    ledger_b.commit_generation(a_id, 0, 1, nonce, now).unwrap();
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
        .send_user(
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
    let bytes = ContentStore::new(b.store())
        .chunk(a_id, "root", content, 0, now)
        .unwrap();
    receiver_a
        .receive_content_chunk(&incoming_a, &reply, 0, &bytes)
        .unwrap();
    receiver_a.accept_cached_reply(&incoming_a, &reply).unwrap();
    receiver_a.accept_cached_reply(&incoming_a, &reply).unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let messages = a.store().messages("root", None, 100).unwrap().items;
        if messages.iter().any(|message| {
            message.kind == super::super::MessageKind::Result
                && message.status == MessageStatus::Completed
        }) {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(caller_model.requests.lock().len(), 4);
    let reply_input = a
        .store()
        .messages("root", None, 100)
        .unwrap()
        .items
        .into_iter()
        .find(|message| message.kind == super::super::MessageKind::Result)
        .unwrap();
    assert_eq!(reply_input.in_reply_to, Some(original.id));
    let mut forbidden = reply.clone();
    forbidden.message_id = uuid::Uuid::new_v4();
    forbidden.body = Body::BlobManifestQuery {
        access: BlobAccess::Actor {
            actor: ActorAddress {
                stage_id: a_id,
                actor_id: "root".into(),
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
        ledger::{ActorGrant, StageLedger},
        protocol::{BlobRef, Body, Envelope, Origin, invocation_id},
        transport::tls,
    };
    let provider = Arc::new(ReplayProvider::new(vec![
        MockReply::Calls(vec![AgentToolCall {
            id: "remote-write".into(),
            name: "files__write_file".into(),
            arguments: json!({"path":"forbidden.txt","content":"must not be written","expected_sha256":null}),
        }]),
        MockReply::Answer("The write was denied."),
    ]));
    let (_directory, system) = runtime(provider).await;
    let mut host = DeviceIdentity::generate().unwrap();
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
    ledger
        .import_peer(
            enrollment,
            vec![ActorGrant {
                actor_id: "root".into(),
                read: true,
                converse: true,
                delegate: false,
                read_only: true,
            }],
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
    let peer = tls::identify_client(&server, &[remote.profile()]).unwrap();
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
            origin: Origin::User,
            target: ActorAddress {
                stage_id: host.profile().stage_id,
                actor_id: "root".into(),
            },
            content: BlobRef {
                hash: crate::stage::digest(content.as_bytes()),
                size: content.len() as u64,
                version: 1,
            },
            attachments: vec![],
        },
    };
    let received = crate::stage::receiver::StageReceiver::new(system.clone())
        .receive_verified_content(&peer, &frame, content)
        .unwrap();
    let message = wait_completed(&system, received.message_id.as_ref().unwrap()).await;
    let receiver = crate::stage::receiver::StageReceiver::new(system.clone());
    let mut query = frame.clone();
    query.message_id = uuid::Uuid::new_v4();
    query.body = Body::ResultQuery {
        invocation: frame.invocation_id.unwrap(),
    };
    let result = receiver.handle_request(&peer, &query).unwrap();
    assert_eq!(result.in_reply_to, Some(query.message_id));
    let Body::Result {
        content: Some(reference),
        ..
    } = result.body
    else {
        panic!("Expected completed result reference");
    };
    query.body = Body::BlobManifestQuery {
        access: crate::stage::protocol::BlobAccess::Actor {
            actor: ActorAddress {
                stage_id: host.profile().stage_id,
                actor_id: "root".into(),
            },
        },
        content: reference.clone(),
    };
    assert!(matches!(
        receiver.handle_request(&peer, &query).unwrap().body,
        Body::BlobManifest { .. }
    ));
    query.body = Body::BlobRead {
        access: crate::stage::protocol::BlobAccess::Actor {
            actor: ActorAddress {
                stage_id: host.profile().stage_id,
                actor_id: "root".into(),
            },
        },
        content: reference,
        chunk: 0,
    };
    let Body::BlobChunk { bytes, .. } = receiver.handle_request(&peer, &query).unwrap().body else {
        panic!("Expected original result bytes");
    };
    assert_eq!(bytes, b"The write was denied.");
    assert_eq!(message.status, MessageStatus::Completed);
    assert!(matches!(
        message.source,
        super::super::MessageSource::RemoteUser { .. }
    ));
    assert!(
        !system
            .actor("root")
            .unwrap()
            .config
            .workspace
            .join("forbidden.txt")
            .exists()
    );
    assert!(message.remote_authority.is_some());
    let result = ledger
        .result(
            remote.profile().stage_id,
            frame.invocation_id.unwrap(),
            chrono::Utc::now().timestamp_millis(),
        )
        .unwrap();
    assert_eq!(
        result.state,
        crate::stage::protocol::InvocationState::Completed
    );
    system.shutdown().await;
}

fn enqueue_root(system: &ActorSystem, content: &str) -> MessageRecord {
    let now = chrono::Utc::now().timestamp_millis();
    let message = system
        .store()
        .enqueue(
            MessageRecord {
                seq: 0,
                id: uuid::Uuid::new_v4().to_string(),
                actor_id: super::super::ROOT_ACTOR_ID.into(),
                source: super::super::MessageSource::User,
                reply_to: super::super::ReplyTarget::User,
                kind: super::super::MessageKind::Request,
                in_reply_to: None,
                delegation_grant: None,
                remote_authority: None,
                content: content.into(),
                attachments: vec![],
                status: MessageStatus::Queued,
                cancel_requested: false,
                final_generation: None,
                started_at_ms: None,
                finished_at_ms: None,
                reply: None,
                error: None,
                created_at_ms: now,
                updated_at_ms: now,
            },
            None,
        )
        .unwrap();
    system.wake(super::super::ROOT_ACTOR_ID);
    message
}

#[tokio::test]
async fn actor_executes_serial_inbox_and_retains_context_across_requests() {
    let provider = Arc::new(ReplayProvider::new(vec![
        MockReply::Answer("first answer"),
        MockReply::Answer("second answer"),
    ]));
    let (_directory, system) = runtime(provider.clone()).await;
    let first = enqueue_root(&system, "first request");
    let second = enqueue_root(&system, "second request");
    assert_eq!(
        wait_completed(&system, &first.id).await.reply.as_deref(),
        Some("first answer")
    );
    assert_eq!(
        wait_completed(&system, &second.id).await.reply.as_deref(),
        Some("second answer")
    );
    {
        let requests = provider.requests.lock();
        assert_eq!(requests.len(), 2);
        assert!(requests[1].messages.iter().any(
        |row| matches!(row, AgentMessage::Assistant { content, .. } if content == "first answer")
    ));
    }
    let events = system.store().events(0, Some("root"), false, 100).unwrap();
    for input in [&first, &second] {
        let streamed: String = events
            .iter()
            .filter_map(|event| match &event.event {
                ActorEvent::GenerationDelta {
                    message_id,
                    channel,
                    text,
                    ..
                } if message_id == &input.id && channel == "assistant" => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(streamed, "small delta ".repeat(50));
    }
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
async fn queued_guidance_updates_the_active_task_without_replaying_completed_tools() {
    let provider = Arc::new(ReplayProvider::new(vec![
        MockReply::Calls(vec![AgentToolCall {
            id: "one-write-before-guidance".into(),
            name: "files__write_file".into(),
            arguments: json!({"path":"guided.txt","content":"write once","expected_sha256":null}),
        }]),
        MockReply::Pending,
        MockReply::Answer("guided answer"),
    ]));
    let (directory, system) = runtime(provider.clone()).await;
    let original = enqueue_root(&system, "Original task constraints.");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if provider.requests.lock().len() >= 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let update = system
        .send_user(
            "root",
            "Continue in Chinese and keep the original constraints.".into(),
            vec![],
            None,
        )
        .unwrap();
    system
        .steer_queued("root", &update.id, &original.id, update.updated_at_ms)
        .await
        .unwrap();
    let result = wait_completed(&system, &original.id).await;
    assert_eq!(result.reply.as_deref(), Some("guided answer"));
    let guided = system.store().message(&update.id).unwrap();
    assert_eq!(guided.kind, super::super::MessageKind::Steering);
    assert_eq!(guided.status, MessageStatus::Completed);
    assert!(guided.reply.is_none());
    assert!(provider.dropped.load(Ordering::Acquire));
    assert_eq!(provider.requests.lock().len(), 3);
    {
        let requests = provider.requests.lock();
        let last = &requests[2].messages;
        assert!(last.iter().any(|row|matches!(row,AgentMessage::Tool{tool_call_id,..} if tool_call_id=="one-write-before-guidance")));
        assert!(last.iter().any(|row|matches!(row,AgentMessage::User{content} if content.as_text().contains("Continue in Chinese"))));
        assert!(last.iter().any(|row|matches!(row,AgentMessage::User{content} if content.as_text().contains("Original task constraints"))));
    }
    assert_eq!(
        std::fs::read_to_string(directory.path().join("workspace/guided.txt")).unwrap(),
        "write once"
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
async fn actor_uses_real_files_app_and_cannot_read_private_runtime_state() {
    let provider = Arc::new(ReplayProvider::new(Vec::new()));
    let (_directory, system) = runtime(provider.clone()).await;
    let workspace = system.store().actor("root").unwrap().config.workspace;
    std::fs::write(workspace.join("inside.txt"), "authorized content\n").unwrap();
    let outside = system.actor_state_dir("root").join("outside.txt");
    std::fs::create_dir_all(outside.parent().unwrap()).unwrap();
    std::fs::write(&outside, "ungranted confidential content\n").unwrap();
    let arguments = |path: String| json!({"path":path,"start_line":null,"line_count":null});
    provider.replies.lock().extend([
        MockReply::Calls(vec![
            AgentToolCall {
                id: "inside-call".into(),
                name: "files__read_file".into(),
                arguments: arguments("inside.txt".into()),
            },
            AgentToolCall {
                id: "outside-call".into(),
                name: "files__read_file".into(),
                arguments: arguments(outside.display().to_string()),
            },
        ]),
        MockReply::Answer("read available data"),
    ]);
    let input = enqueue_root(&system, "read the two paths");
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
        if tool_call_id == "inside-call" && content.contains("authorized content"))
        ));
        assert!(follow_up.iter().any(
            |row| matches!(row, AgentMessage::Tool { tool_call_id, content, .. }
        if tool_call_id == "outside-call" && content.contains("App error"))
        ));
        assert!(
            !follow_up
                .iter()
                .any(|row| matches!(row, AgentMessage::Tool { content, .. }
        if content.contains("ungranted confidential content")))
        );
    }
    system.shutdown().await;
}

async fn paused_runtime() -> (tempfile::TempDir, ActorSystem, Arc<ReplayProvider>) {
    let directory = tempfile::tempdir().unwrap();
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
    let mut root = system.actor("root").unwrap();
    root.config.permissions.approval_mode = super::super::ApprovalMode::None;
    system.store().save_actor(&root).unwrap();
    (directory, system, provider)
}

fn project_config(system: &ActorSystem, name: &str, read_only: bool) -> ActorConfig {
    let workspace = system.actor("root").unwrap().config.workspace.join(name);
    let mut permissions = super::super::ActorPermissions::workspace(workspace.clone(), read_only);
    permissions.approval_mode = super::super::ApprovalMode::None;
    ActorConfig {
        name: name.into(),
        workspace,
        apps: vec!["files".into()],
        model: None,
        instructions: format!("Instructions for {name}"),
        permissions,
    }
}

#[tokio::test]
async fn delegated_and_direct_inputs_keep_their_original_reply_targets() {
    use super::super::{MessageKind, ReplyTarget};
    let (_directory, system, _) = paused_runtime().await;
    let child = system
        .create_owned("root", project_config(&system, "project", false))
        .unwrap();
    let final_constraint =
        "Full original constraint: retain local data and never publish the report";
    let original = system
        .send_user(
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
        .send_from_actor("root", &original.id, &child.id, "Delegated work".into())
        .unwrap();
    let direct = system
        .send_user(&child.id, "Separate direct user work".into(), vec![], None)
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
        .send_user("root", "A newer independent request".into(), vec![], None)
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
        .store()
        .finish_message(
            &delegated.id,
            MessageStatus::Completed,
            Some("Delegated result"),
            None,
        )
        .unwrap()
        .unwrap();
    assert_eq!(routed.actor_id, "root");
    assert_eq!(routed.kind, MessageKind::Result);
    assert_eq!(routed.in_reply_to.as_deref(), Some(original.id.as_str()));
    assert!(matches!(routed.reply_to, ReplyTarget::User));
    assert_eq!(
        system.store().claim_next(&child.id).unwrap().unwrap().id,
        direct.id
    );
    assert!(
        system
            .store()
            .finish_message(
                &direct.id,
                MessageStatus::Completed,
                Some("Direct user result"),
                None
            )
            .unwrap()
            .is_none()
    );
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
    use super::super::{ApprovalDecision, ApprovalMode, ApprovalRecord, ToolPermissionRequest};
    let (directory, system, provider) = paused_runtime().await;
    let mut config = project_config(&system, "persistent", false);
    config.permissions.approval_mode = ApprovalMode::Ask;
    let child = system.create_owned("root", config).unwrap();
    let completed = system
        .send_user(
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
        .send_user(
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
            seq: 0,
            id: uuid::Uuid::new_v4().to_string(),
            actor_id: child.id.clone(),
            message_id: active.id.clone(),
            call: pending_call,
            requested: ToolPermissionRequest::default(),
            mode: ApprovalMode::Ask,
            decision: ApprovalDecision::Pending,
            reason: "Pending owner approval".into(),
            created_at_ms: chrono::Utc::now().timestamp_millis(),
            resolved_at_ms: None,
        })
        .unwrap();
    let queued = system
        .send_user(&child.id, "Queued work survives".into(), vec![], None)
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
async fn read_only_actor_cannot_delegate_new_write_requests_to_stronger_actors() {
    let (_directory, system, _) = paused_runtime().await;
    let writable = system
        .create_owned("root", project_config(&system, "writable", false))
        .unwrap();
    let mut research_config = project_config(&system, "research", true);
    research_config.apps.push("actors".into());
    let reader_workspace = research_config.workspace.join("reader");
    let mut reader_config = research_config.clone();
    reader_config.name = "Reader before upgrade".into();
    reader_config.apps = vec!["files".into()];
    reader_config.workspace = reader_workspace.clone();
    reader_config.permissions = super::super::ActorPermissions::workspace(reader_workspace, true);
    reader_config.permissions.approval_mode = super::super::ApprovalMode::None;
    let reader = system.create_owned("root", reader_config).unwrap();
    research_config
        .permissions
        .message_targets
        .push(writable.id.clone());
    research_config
        .permissions
        .message_targets
        .push(reader.id.clone());
    let research = system.create_owned("root", research_config).unwrap();
    let input = system
        .send_user(&research.id, "Investigate the project".into(), vec![], None)
        .unwrap();
    system.store().claim_next(&research.id).unwrap().unwrap();
    assert!(
        system
            .send_from_actor(&research.id, &input.id, "root", "Write on my behalf".into())
            .is_err()
    );
    assert!(
        system
            .send_from_actor(
                &research.id,
                &input.id,
                &writable.id,
                "Write on my behalf".into()
            )
            .is_err()
    );
    assert!(
        system
            .store()
            .messages("root", None, 100)
            .unwrap()
            .items
            .is_empty()
    );
    assert!(
        system
            .store()
            .messages(&writable.id, None, 100)
            .unwrap()
            .items
            .is_empty()
    );
    let context = system
        .execution_context(&research, &system.store().message(&input.id).unwrap())
        .unwrap();
    assert!(
        system
            .requested_tool_context(
                &context,
                &super::super::ToolPermissionRequest {
                    operation_details: serde_json::Value::Null,
                    readable_roots: vec![],
                    writable_roots: vec![writable.config.workspace.clone()]
                }
            )
            .is_err()
    );
    let delegated = system
        .send_from_actor(
            &research.id,
            &input.id,
            &reader.id,
            "Read the requested files".into(),
        )
        .unwrap();
    assert!(delegated.delegation_grant.is_some());
    assert!(
        serde_json::to_value(&delegated)
            .unwrap()
            .get("delegation_grant")
            .is_none(),
        "internal capabilities must never be exposed in the message API"
    );
    let snapshot = system
        .store()
        .message(&delegated.id)
        .unwrap()
        .delegation_grant;
    let mut upgraded = reader.config.clone();
    upgraded.permissions.read_only = false;
    upgraded
        .permissions
        .writable_roots
        .push(upgraded.workspace.clone());
    system.update_owned(&reader.id, upgraded).await.unwrap();
    let active_receiver = system.store().claim_next(&reader.id).unwrap().unwrap();
    assert_eq!(active_receiver.id, delegated.id);
    assert!(
        active_receiver.delegation_grant.is_some(),
        "claim/save must preserve the immutable delegation ceiling"
    );
    assert_eq!(
        serde_json::to_value(&snapshot).unwrap(),
        serde_json::to_value(&active_receiver.delegation_grant).unwrap()
    );
    assert!(
        system
            .execution_context(&system.actor(&reader.id).unwrap(), &active_receiver)
            .is_err(),
        "a queued read-only request must not acquire the recipient's newly granted writes"
    );
    system.shutdown().await;
}

#[tokio::test]
async fn broad_operation_scope_cannot_read_future_actors_delivery_data() {
    let (_directory, system, _) = paused_runtime().await;
    let input = system
        .send_user("root", "Inspect allowed files".into(), vec![], None)
        .unwrap();
    let active = system.store().claim_next("root").unwrap().unwrap();
    assert_eq!(active.id, input.id);
    let root = system.actor("root").unwrap();
    let base = system.execution_context(&root, &active).unwrap();
    let drive = root
        .config
        .workspace
        .ancestors()
        .last()
        .unwrap()
        .to_path_buf();
    let broad = system.requested_tool_context(
        &base,
        &super::super::ToolPermissionRequest {
            operation_details: serde_json::Value::Null,
            readable_roots: vec![drive],
            writable_roots: vec![],
        },
    );
    let future = system
        .create_owned("root", project_config(&system, "future", false))
        .unwrap();
    let secret = system.actor_data_dir(&future.id).join("private.txt");
    std::fs::write(&secret, "future Actor data").unwrap();
    match broad {
        Ok(context) => assert!(
            !context.sandbox_policy.is_path_readable(&secret),
            "the hard delivery boundary must protect Actors created after the grant"
        ),
        Err(error) => assert!(matches!(
            error.downcast_ref::<super::super::ActorError>(),
            Some(super::super::ActorError::Forbidden(_))
        )),
    }
    assert_eq!(system.actor("root").unwrap().config, root.config);
    system.shutdown().await;
}

#[tokio::test]
async fn deep_actor_artifact_results_are_rebound_to_parent_resource_scope() {
    use super::super::StoredResource;
    let (_directory, system, _) = paused_runtime().await;
    let mut parent_config = project_config(&system, "parent", false);
    parent_config.apps.push("actors".into());
    let parent = system.create_owned("root", parent_config).unwrap();
    let original = system
        .send_user("root", "Produce an HTML report".into(), vec![], None)
        .unwrap();
    system.store().claim_next("root").unwrap().unwrap();
    let delegated = system
        .send_from_actor("root", &original.id, &parent.id, "Create the report".into())
        .unwrap();
    let active_parent = system.store().claim_next(&parent.id).unwrap().unwrap();
    let parent_context = system.execution_context(&parent, &active_parent).unwrap();
    let mut config = parent.config.clone();
    config.name = "report worker".into();
    config.workspace = parent.config.workspace.join("report-worker");
    config.permissions = super::super::ActorPermissions::workspace(config.workspace.clone(), false);
    config.permissions.approval_mode = super::super::ApprovalMode::None;
    let deep = system
        .create_from_actor(&parent.id, config, &parent_context)
        .unwrap();
    assert_eq!(deep.depth, 2);
    assert!(
        system
            .list_visible()
            .unwrap()
            .iter()
            .any(|actor| actor.id == deep.id)
    );
    let request = system
        .send_from_actor(&parent.id, &delegated.id, &deep.id, "Write report".into())
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
        .store()
        .finish_message(
            &request.id,
            MessageStatus::Completed,
            Some(&format!("[Report]({uri})")),
            None,
        )
        .unwrap()
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
        .store()
        .finish_message(
            &to_parent.id,
            MessageStatus::Completed,
            Some(&to_parent.content),
            None,
        )
        .unwrap()
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
        .send_user(
            &deep.id,
            "Direct owner question through the Actor link".into(),
            vec![],
            None,
        )
        .unwrap();
    assert_eq!(direct.actor_id, deep.id);
    assert!(matches!(direct.reply_to, super::super::ReplyTarget::User));
    system.shutdown().await;
}

#[tokio::test]
async fn reconfiguration_stops_active_work_preserves_queue_and_keeps_one_worker() {
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
    invalid.workspace = system.actor_state_dir("root");
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
    tokio::time::timeout(
        Duration::from_secs(3),
        system.update_owned("root", next.clone()),
    )
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
            matches!(requests[0].messages.last(), Some(AgentMessage::User { content }) if content.as_text().contains(&active.id))
        );
        assert!(
            matches!(requests[1].messages.last(), Some(AgentMessage::User { content }) if content.as_text().contains(&queued.id))
        );
    }
    system.shutdown().await;
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
                path: "owned.png".to_owned(),
                media_type: "image/png".to_owned(),
                description: None,
            }],
        )),
    ];
    let window = model_window(
        &BudgetProvider { tokens: 10_000 },
        "system",
        &history,
        &[],
        &TokenEstimateBaseline::default(),
    )
    .unwrap();
    let window = window.unwrap();
    assert!(window.messages[2] == protocol);
    assert_eq!(
        window.messages[2].responses_output().unwrap()[0]["encrypted_content"],
        "ciphertext"
    );
    assert_eq!(window.messages.len(), 6);
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
        model_window(
            &BudgetProvider { tokens: 500 },
            "system",
            &history,
            &[],
            &TokenEstimateBaseline::default()
        )
        .unwrap()
        .is_none()
    );
    assert_eq!(history.len(), 4);
}

#[test]
fn oversized_current_input_is_archived_for_recovery() {
    let history = vec![AgentMessage::user("z".repeat(8_000))];
    assert!(
        model_window(
            &BudgetProvider { tokens: 100 },
            "system",
            &history,
            &[],
            &TokenEstimateBaseline::default()
        )
        .unwrap()
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
fn duplicate_calls_are_rejected_before_any_effect() {
    let response = AgentTurnResponse {
        tool_calls: vec![call("same"), call("same")],
        assistant_text: None,
        final_assistant_message: None,
        reasoning_content: None,
        reasoning_signature: None,
        responses_output: None,
        raw_stream_follow_up: true,
    };
    assert!(validate_response_calls(&response).is_err());
}

fn fixture_enrollment(
    host: &crate::stage::host::Host,
    request: &crate::stage::identity::JoinRequest,
    can_enroll: bool,
    grants: Vec<crate::stage::ledger::ActorGrant>,
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
        grants,
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
        .send_user("root", "keep working".into(), vec![], None)
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
        .send_user("root", "continue".into(), vec![], None)
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
    let provider = Arc::new(ReplayProvider::new(vec![MockReply::Answer("recovered")]));
    let (_dir, system) = runtime(provider.clone()).await;
    let original = AgentMessage::assistant("OLD_MODEL_WINDOW".repeat(40000));
    system
        .store()
        .append_history("root", "past", std::slice::from_ref(&original))
        .unwrap();
    let message = system
        .send_user("root", "recover this task".into(), vec![], None)
        .unwrap();
    let result = wait_completed(&system, &message.id).await;
    assert_eq!(
        result.status,
        MessageStatus::Completed,
        "{:?}",
        result.error
    );
    let requests = provider.requests.lock().clone();
    assert_eq!(requests[0].messages.len(), 2);
    assert!(requests[0].messages.iter().any(|row|matches!(row,AgentMessage::User{content} if content.as_text().contains("read_history"))));
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
    assert!(system.store().context_start("root").unwrap() > 0);
    drop(requests);
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
        model_window(
            &BudgetProvider { tokens: 500 },
            "system",
            &history,
            &[],
            &low
        )
        .unwrap()
        .is_some()
    );
    let high = TokenEstimateBaseline {
        estimated_input_tokens: 100,
        observed_input_tokens: Some(450),
    };
    assert!(
        model_window(
            &BudgetProvider { tokens: 500 },
            "system",
            &history,
            &[],
            &high
        )
        .unwrap()
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
        .send_user("root", "first".into(), vec![], None)
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
        context_baseline(&reopened, "root", "another-model").unwrap(),
        TokenEstimateBaseline::default()
    );
    provider.usage_bias.store(0, Ordering::Relaxed);
    let second = reopened
        .send_user("root", "second".into(), vec![], None)
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
async fn history_app_is_own_actor_scoped_paged_and_has_no_provider_reasoning() {
    use crate::{actor_app::ActorApp, app::App};
    let provider = Arc::new(ReplayProvider::new(vec![MockReply::Pending]));
    let (_dir, system) = runtime(provider.clone()).await;
    let child = system
        .create_owned("root", project_config(&system, "other actor", false))
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
        .send_user("root", "wait".into(), vec![], None)
        .unwrap();
    provider.started.notified().await;
    let actor = system.actor("root").unwrap();
    let context = system.execution_context(&actor, &message).unwrap();
    let mut app = ActorApp::history_only("root".into(), system.clone());
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
    query.name = "create".into();
    assert!(app.before_tool_call(&query, &context).is_err());
    system.shutdown().await;
}

async fn start_core_fixture(system: &ActorSystem) -> Arc<crate::stage::runtime::Runtime> {
    let host = crate::stage::host::Host::load(system.clone()).unwrap();
    host.initialize(true).unwrap();
    let mut settings = host.settings();
    settings.interfaces = vec!["fixture-no-lan-interface".into()];
    host.save_settings(settings).unwrap();
    let network = crate::stage::runtime::Runtime::new(host);
    network.start().await.unwrap();
    tokio::time::timeout(Duration::from_secs(30), async {
        while !network.core.read().as_ref().unwrap().permitted() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    network
}

#[tokio::test]
async fn core_delivery_is_visible_before_the_originating_turn_finishes() {
    let provider = Arc::new(ReplayProvider::new(vec![
        MockReply::Calls(vec![AgentToolCall {
            id: "early".into(),
            name: "actors__deliver".into(),
            arguments: json!({"content":"I have started coordinating.","finish_turn":false}),
        }]),
        MockReply::Pending,
    ]));
    let (_dir, system) = runtime(provider.clone()).await;
    let network = start_core_fixture(&system).await;
    assert!(network.core.read().as_ref().unwrap().accepts_input());
    let input = system
        .send_user("core", "Start".into(), vec![], None)
        .unwrap();
    tokio::time::timeout(Duration::from_secs(30), async {
        while provider.requests.lock().len() < 2 {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        system.store().message(&input.id).unwrap().status,
        MessageStatus::Running
    );
    let receipt = system
        .store()
        .delivery_receipt("core", &input.id, "early")
        .unwrap()
        .expect("committed delivery");
    assert_eq!(
        receipt.reply.as_deref(),
        Some("I have started coordinating.")
    );
    assert_eq!(receipt.in_reply_to.as_deref(), Some(input.id.as_str()));
    assert!(system.store().events(0,Some("core"),true,100).unwrap().iter().any(|event|matches!(&event.event,ActorEvent::MessageAdded{message_id} if message_id==&receipt.id)));
    {
        let requests = provider.requests.lock();
        assert!(
            requests[0]
                .tools
                .iter()
                .all(|tool| tool.name.starts_with("actors__"))
        );
        assert!(
            requests[0]
                .tools
                .iter()
                .any(|tool| tool.name == "actors__deliver")
        );
    }
    system.cancel("core", &input.id).unwrap();
    assert!(
        system
            .deliver_from_actor("core", &input.id, "late", "Too late")
            .is_err()
    );
    network.stop().await;
    system.shutdown().await;
}

struct CoordinatorFactory {
    core: Arc<ReplayProvider>,
    worker: Arc<ReplayProvider>,
}
impl ModelFactory for CoordinatorFactory {
    fn build(&self, name: &str, _: &crate::config::Config) -> Result<Arc<dyn ModelProvider>> {
        Ok(if name == "worker" {
            self.worker.clone()
        } else {
            self.core.clone()
        })
    }
}
#[tokio::test]
async fn core_yields_after_delegating_and_receives_progress_while_worker_runs() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let core = Arc::new(ReplayProvider::new(vec![]));
    let worker = Arc::new(ReplayProvider::new(vec![
        MockReply::Calls(vec![AgentToolCall {
            id: "progress".into(),
            name: "actors__report".into(),
            arguments: json!({"content":"Found the cause; repair is still running."}),
        }]),
        MockReply::Pending,
    ]));
    let mut config = runtime_config();
    config
        .models
        .insert("worker".into(), config.models["mock"].clone());
    let system = ActorSystem::open_with_factory(
        dir.path().join("data"),
        workspace,
        config,
        Arc::new(CoordinatorFactory {
            core: core.clone(),
            worker: worker.clone(),
        }),
    )
    .await
    .unwrap();
    let mut root = system.actor("root").unwrap();
    root.config.permissions.approval_mode = super::super::ApprovalMode::None;
    system.store().save_actor(&root).unwrap();
    let network = start_core_fixture(&system).await;
    let mut child_config = project_config(&system, "Hosted worker", false);
    child_config.model = Some("worker".into());
    let child = system.create_owned("root", child_config).unwrap();
    let stage = system.store().public_stage_id().unwrap();
    core.replies.lock().extend([
        MockReply::Calls(vec![
            AgentToolCall{id:"delegate".into(),name:"actors__send".into(),arguments:json!({"stage_id":stage,"actor_id":child.id,"content":"Investigate and repair"})},
            AgentToolCall{id:"ack".into(),name:"actors__deliver".into(),arguments:json!({"content":"The worker is investigating.","finish_turn":true})},
        ]),
        MockReply::Answer("Your second message has been handled."),
        MockReply::Calls(vec![AgentToolCall{id:"update".into(),name:"actors__deliver".into(),arguments:json!({"content":"The cause is known; repair continues.","finish_turn":true})}]),
    ]);
    let first = system
        .send_user("core", "Investigate".into(), vec![], None)
        .unwrap();
    let second = system
        .send_user("core", "Meanwhile, answer this".into(), vec![], None)
        .unwrap();
    let done = wait_core_completed(&system, &first.id).await;
    assert_eq!(done.status, MessageStatus::Completed, "{:?}", done.error);
    assert_eq!(done.reply.as_deref(), Some(""));
    assert_eq!(
        wait_core_completed(&system, &second.id)
            .await
            .reply
            .as_deref(),
        Some("Your second message has been handled.")
    );
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let messages = system.store().messages("core", None, 100).unwrap().items;
            if messages.iter().any(|m| {
                m.kind == MessageKind::Delivery
                    && m.reply.as_deref() == Some("The cause is known; repair continues.")
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        core.requests.lock().len(),
        3,
        "finish_turn must not make an extra inference"
    );
    let task = system
        .store()
        .running_message(&child.id)
        .unwrap()
        .expect("worker remains active");
    assert_eq!(
        task.source,
        MessageSource::Actor {
            actor_id: "core".into()
        }
    );
    let progress = system
        .store()
        .messages("core", None, 100)
        .unwrap()
        .items
        .into_iter()
        .find(|m| m.kind == MessageKind::Progress)
        .unwrap();
    assert_eq!(progress.in_reply_to.as_deref(), Some(first.id.as_str()));
    assert_eq!(
        progress.source,
        MessageSource::Actor {
            actor_id: child.id.clone()
        }
    );
    assert!(progress.content.contains("repair is still running"));
    assert_eq!(system.actor("core").unwrap().config.apps, vec!["actors"]);
    assert!(
        crate::actors::permissions::delegation_apps(&system.core_device_actor().unwrap().config)
            .contains(&"files".into())
    );
    network.stop().await;
    system.shutdown().await;
}

// Real account coordination can lose its short grant while another native
// build monopolizes this test host. Allow election/quarantine to finish; the
// assertion still requires both user inputs to finish while the child is active.
async fn wait_core_completed(system: &ActorSystem, id: &str) -> MessageRecord {
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let message = system.store().message(id).unwrap();
            if !matches!(
                message.status,
                MessageStatus::Queued | MessageStatus::Running
            ) {
                return message;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap_or_else(|error| {
        panic!(
            "Core did not finish: {error}; message={:?}; core={:?}",
            system.store().message(id),
            system.shared_core().map(|core| core.status())
        )
    })
}
