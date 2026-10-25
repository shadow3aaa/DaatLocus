use super::*;

use crate::{
    config::{Config, ModelConfig, ProviderConfig},
    context_budget::RequestBudgetLimits,
    core::{ModelProvider, TokenUsageInfo},
    model_protocol::{AgentTurnRequest, AgentTurnStreamResult},
};
use async_trait::async_trait;
use std::sync::{Arc, atomic::AtomicBool};

enum ReviewReply {
    Json(serde_json::Value),
    Blocked(Arc<tokio::sync::Notify>, serde_json::Value),
    Fail,
    Timeout,
    Pending,
}
struct Reviewer {
    reply: parking_lot::Mutex<Option<ReviewReply>>,
    requests: parking_lot::Mutex<Vec<PromptRequest>>,
    started: tokio::sync::Notify,
    dropped: Arc<AtomicBool>,
}
impl Reviewer {
    fn new(reply: ReviewReply) -> Self {
        Self {
            reply: parking_lot::Mutex::new(Some(reply)),
            requests: parking_lot::Mutex::new(vec![]),
            started: tokio::sync::Notify::new(),
            dropped: Arc::new(AtomicBool::new(false)),
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
impl ModelProvider for Reviewer {
    async fn complete_json(
        &self,
        request: PromptRequest,
        options: ModelRequestOptions,
    ) -> Result<serde_json::Value> {
        assert!(
            options.conversation_id.is_none(),
            "reviewer must not reuse the executing conversation"
        );
        self.requests.lock().push(request);
        self.started.notify_one();
        let reply = self.reply.lock().take().expect("one reviewer request");
        match reply {
            ReviewReply::Json(value) => Ok(value),
            ReviewReply::Blocked(release, value) => {
                release.notified().await;
                Ok(value)
            }
            ReviewReply::Fail => Err(miette!("mock review unavailable")),
            ReviewReply::Timeout => Err(crate::providers::ModelRequestFailure::report(
                crate::providers::ModelFailureKind::Timeout,
                "fixture provider reached its configured timeout".into(),
            )),
            ReviewReply::Pending => {
                let _drop = DropMarker(self.dropped.clone());
                std::future::pending().await
            }
        }
    }
    async fn complete_agent_turn(
        &self,
        _: AgentTurnRequest,
        _: ModelRequestOptions,
    ) -> Result<AgentTurnStreamResult> {
        panic!("permission reviewer cannot execute Actor turns");
    }
    fn request_budget_limits(&self) -> RequestBudgetLimits {
        RequestBudgetLimits {
            context_window_tokens: 100_000,
            auto_compact_threshold_tokens: 100_000,
            reserved_output_tokens: 2_000,
        }
    }
    fn token_usage_info(&self) -> TokenUsageInfo {
        TokenUsageInfo::default()
    }
    fn model_name(&self) -> String {
        "mock-reviewer".into()
    }
}
struct Factory(Arc<Reviewer>);
impl super::super::ModelFactory for Factory {
    fn build(&self, name: &str, _: &Config) -> Result<Arc<dyn ModelProvider>> {
        assert_eq!(
            name, "reviewer",
            "use the independently selected approval model"
        );
        Ok(self.0.clone())
    }
}

use crate::actors::ReviewApplication;
use crate::actors::runtime_permissions::{self as runtime, ApplicationSource};
use uuid::Uuid;

async fn fixture(
    reviewer: Arc<Reviewer>,
) -> (tempfile::TempDir, ActorSystem, ActorRecord, MessageRecord) {
    let directory = crate::sandbox::test_support::scoped_directory();
    let workspace = directory.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let mut config = Config {
        main_model: "main".into(),
        approval_model: Some("reviewer".into()),
        ..Default::default()
    };
    config.models.insert(
        "main".into(),
        ModelConfig {
            provider: "mock".into(),
            model_id: "mock".into(),
            ..Default::default()
        },
    );
    config
        .models
        .insert("reviewer".into(), config.models["main"].clone());
    config.providers.insert(
        "mock".into(),
        ProviderConfig::OpenaiCompatible {
            base_url: "https://example.invalid".into(),
            api_key: "unused-test-key".into(),
        },
    );
    let system = ActorSystem::open_paused_with_factory(
        directory.path().join("data"),
        workspace,
        config,
        Arc::new(Factory(reviewer)),
    )
    .await
    .unwrap();
    system.install_local_core().unwrap();
    system.seed_test_actor("root");
    let actor = system.actor("root").unwrap();
    let input = input(&system, &actor.id);
    (directory, system, actor, input)
}

fn input(system: &ActorSystem, actor: &str) -> MessageRecord {
    let message: MessageRecord = serde_json::from_value(json!({
        "seq":0,"id":Uuid::new_v4().to_string(),"actor_id":actor,"source":{"kind":"user"},
        "reply_to":{"kind":"user"},"kind":"request","in_reply_to":null,"remote_authority":null,
        "content":"Read the requested directory and write the requested report.","attachments":[],
        "status":"queued","cancel_requested":false,"reply":null,"error":null,"created_at_ms":1,"updated_at_ms":1
    })).unwrap();
    system.store().enqueue(message, None).unwrap();
    system.store().claim_next(actor).unwrap().unwrap()
}

fn source(
    system: &ActorSystem,
    actor: &ActorRecord,
    message: &MessageRecord,
    read_only: bool,
) -> (AgentToolCall, String) {
    let call = AgentToolCall {
        id: Uuid::new_v4().to_string(),
        name: "fixture__inspect".into(),
        arguments: json!({"target":"original source operation"}),
    };
    let id = system
        .store()
        .tool_started_with_authority(&actor.id, &message.id, &call, read_only, true)
        .unwrap();
    system
        .store()
        .tool_finished_with_reference(
            &actor.id,
            &message.id,
            &call,
            json!({"original":"unchanged output"}),
            "unchanged output",
            "Source observation",
            None,
            Some(&id),
        )
        .unwrap();
    (call, id)
}

fn application() -> ReviewApplication {
    ReviewApplication {
        task: "Complete the requested report".into(),
        reason: "Access the exact source directory required by the request".into(),
    }
}

fn scope_request(result: &str, directory: &std::path::Path, write: bool) -> ApplicationSource {
    ApplicationSource::ToolResult {
        tool_result_id: result.into(),
        readable_roots: if write {
            vec![]
        } else {
            vec![directory.into()]
        },
        writable_roots: if write {
            vec![directory.into()]
        } else {
            vec![]
        },
    }
}

fn core_inputs(system: &ActorSystem) -> Vec<MessageRecord> {
    system.store().messages("core", None, 100).unwrap().items
}

#[tokio::test]
async fn approved_application_blocks_for_review_then_grants_device_scope_without_core_or_replay() {
    let reviewer = Arc::new(Reviewer::new(ReviewReply::Json(
        json!({"decision":"approve","reason":"required scope"}),
    )));
    let (directory, system, actor, message) = fixture(reviewer.clone()).await;
    let outside = directory.path().join("source");
    std::fs::create_dir_all(&outside).unwrap();
    let marker = outside.join("effects.txt");
    std::fs::write(&marker, "one original effect").unwrap();
    let (_, result) = source(&system, &actor, &message, false);
    let base = system.execution_context(&actor, &message).unwrap();
    assert!(!base.sandbox_policy.is_path_writable(&outside));
    let approved = runtime::submit(
        &system,
        &actor,
        &message,
        &ActorControl::default(),
        &base,
        scope_request(&result, &outside, true),
        application(),
    )
    .await
    .unwrap();
    assert_eq!(approved.decision, ApprovalDecision::Approved);
    assert!(approved.appeal.is_none());
    assert!(core_inputs(&system).is_empty());
    assert_eq!(reviewer.requests.lock().len(), 1);
    {
        let requests = reviewer.requests.lock();
        assert!(requests[0].history_messages.is_empty());
    }
    system.seed_test_actor("other");
    let other = system.actor("other").unwrap();
    let other_input = input(&system, &other.id);
    let current = system.execution_context(&other, &other_input).unwrap();
    assert!(
        current
            .sandbox_policy
            .is_path_writable(&outside.join("future/nested/report.txt"))
    );
    assert!(
        !current
            .sandbox_policy
            .is_path_writable(&directory.path().join("unrequested"))
    );
    assert_eq!(
        std::fs::read_to_string(marker).unwrap(),
        "one original effect"
    );
    system.shutdown().await;
}

#[tokio::test]
async fn concurrent_applications_share_one_source_record_and_one_automatic_review() {
    let release = Arc::new(tokio::sync::Notify::new());
    let reviewer = Arc::new(Reviewer::new(ReviewReply::Blocked(
        release.clone(),
        json!({"decision":"approve","reason":"required scope"}),
    )));
    let (directory, system, actor, message) = fixture(reviewer.clone()).await;
    let outside = directory.path().join("source");
    std::fs::create_dir(&outside).unwrap();
    let (_, result) = source(&system, &actor, &message, false);
    let base = system.execution_context(&actor, &message).unwrap();
    let control = ActorControl::default();
    let submit = || {
        runtime::submit(
            &system,
            &actor,
            &message,
            &control,
            &base,
            scope_request(&result, &outside, false),
            application(),
        )
    };
    let unblock = async {
        reviewer.started.notified().await;
        release.notify_one();
    };
    let (first, second, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(submit(), submit(), unblock)
    })
    .await
    .expect("duplicate application did not share the pending review");
    let first = first.unwrap();
    let second = second.unwrap();
    assert_eq!(first.id, second.id);
    assert_eq!(first.decision, ApprovalDecision::Approved);
    assert_eq!(reviewer.requests.lock().len(), 1);
    assert!(core_inputs(&system).is_empty());
    assert_eq!(
        system
            .store()
            .approvals(Some(&actor.id), None, 10)
            .unwrap()
            .items
            .len(),
        1
    );
    system.shutdown().await;
}

#[tokio::test]
async fn denied_application_forwards_asynchronously_and_reusing_the_incident_never_reviews_again() {
    let reviewer = Arc::new(Reviewer::new(ReviewReply::Json(
        json!({"decision":"deny","reason":"ask Owner"}),
    )));
    let (directory, system, actor, message) = fixture(reviewer.clone()).await;
    let outside = directory.path().join("source");
    std::fs::create_dir_all(&outside).unwrap();
    let (_, result) = source(&system, &actor, &message, false);
    let base = system.execution_context(&actor, &message).unwrap();
    let control = ActorControl::default();
    let submit = || {
        runtime::submit(
            &system,
            &actor,
            &message,
            &control,
            &base,
            scope_request(&result, &outside, false),
            application(),
        )
    };
    let record = submit().await.unwrap();
    assert_eq!(record.decision, ApprovalDecision::Denied);
    assert!(record.appeal.is_some());
    assert_eq!(core_inputs(&system).len(), 1);
    assert!(
        !system
            .execution_context(&actor, &message)
            .unwrap()
            .sandbox_policy
            .is_path_readable(&outside)
    );
    let again = submit().await.unwrap();
    assert_eq!(again.id, record.id);
    let nested = outside.join("narrower");
    std::fs::create_dir(&nested).unwrap();
    let narrowed = runtime::submit(
        &system,
        &actor,
        &message,
        &control,
        &base,
        scope_request(&result, &nested, false),
        application(),
    )
    .await
    .unwrap();
    assert_ne!(narrowed.id, record.id);
    assert_eq!(narrowed.previous_denials, vec![record.id.clone()]);
    assert_eq!(
        narrowed.requested.readable_roots,
        vec![crate::actors::permissions::canonical_path(&nested).unwrap()]
    );
    assert!(narrowed.appeal.is_some());
    let narrowed_again = runtime::submit(
        &system,
        &actor,
        &message,
        &control,
        &base,
        scope_request(&result, &nested, false),
        application(),
    )
    .await
    .unwrap();
    assert_eq!(narrowed_again.id, narrowed.id);
    for (path, write) in [(outside.as_path(), true), (directory.path(), false)] {
        let expanded = runtime::submit(
            &system,
            &actor,
            &message,
            &control,
            &base,
            scope_request(&result, path, write),
            application(),
        )
        .await
        .unwrap();
        assert_eq!(expanded.previous_denials, vec![record.id.clone()]);
        assert!(expanded.appeal.is_some());
    }
    // Core receives the narrowed application with its own immutable scope.
    // Approving it must not revive the larger refused directory.
    let core_input = system.store().claim_next("core").unwrap().unwrap();
    let core = system.core_device_actor().unwrap();
    let caller = system
        .execution_context(&core, &core_input)
        .unwrap()
        .actor_call
        .unwrap();
    system
        .resolve_appeal_from_core(
            &caller,
            &narrowed.id,
            ApprovalDecision::Approved,
            "Owner authorized only the narrower directory",
        )
        .await
        .unwrap();
    let policy = system
        .execution_context(&actor, &message)
        .unwrap()
        .sandbox_policy;
    assert!(policy.is_path_readable(&nested.join("future.txt")));
    assert!(!policy.is_path_readable(&outside.join("sibling.txt")));
    assert!(!policy.is_path_writable(&nested.join("future.txt")));
    assert_eq!(reviewer.requests.lock().len(), 1);
    // An independent scope still receives automatic review and can be granted
    // without adding another Core input.
    let disjoint = directory.path().join("independent");
    std::fs::create_dir(&disjoint).unwrap();
    *reviewer.reply.lock() = Some(ReviewReply::Json(json!({
        "decision":"approve", "reason":"independent scope is authorized"
    })));
    let independent = runtime::submit(
        &system,
        &actor,
        &message,
        &control,
        &base,
        scope_request(&result, &disjoint, true),
        application(),
    )
    .await
    .unwrap();
    assert!(independent.previous_denials.is_empty());
    assert_eq!(independent.decision, ApprovalDecision::Approved);
    assert!(independent.appeal.is_none());
    let from_approval = runtime::submit(
        &system,
        &actor,
        &message,
        &ActorControl::default(),
        &base,
        ApplicationSource::Approval {
            approval_id: record.id.clone(),
        },
        application(),
    )
    .await
    .unwrap();
    assert_eq!(from_approval.id, record.id);
    assert_eq!(reviewer.requests.lock().len(), 2);
    assert_eq!(core_inputs(&system).len(), 4);
    system.shutdown().await;
}

#[tokio::test]
async fn atomic_write_reviews_the_first_missing_directory_before_creating_its_hierarchy() {
    let release = Arc::new(tokio::sync::Notify::new());
    let reviewer = Arc::new(Reviewer::new(ReviewReply::Blocked(
        release.clone(),
        json!({"decision":"approve","reason":"create the requested report directory"}),
    )));
    let (directory, system, actor, message) = fixture(reviewer.clone()).await;
    let missing = directory.path().join("new-output");
    let file = missing.join("nested/report.txt");
    let (call, _) = source(&system, &actor, &message, false);
    let control = ActorControl::default();
    let mut context = system.execution_context(&actor, &message).unwrap();
    runtime::bind_access_broker(&mut context, &system, &actor, &message, &call, &control);
    let filesystem = crate::sandbox::access::FileSystem::new(&context);
    let args = crate::file_io::WriteFileArgs {
        path: file.to_string_lossy().into_owned(),
        content: "report output".into(),
        expected_sha256: None,
    };
    let operation = filesystem.write_file(&args);
    tokio::pin!(operation);
    tokio::select! {
        _ = reviewer.started.notified() => {},
        result = &mut operation => panic!("write returned before review: {result:?}"),
    }
    assert!(!missing.exists(), "review must precede directory creation");
    release.notify_one();
    operation
        .await
        .expect("the same atomic write must resume after approving its actual creation boundary");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "report output");
    assert_eq!(reviewer.requests.lock().len(), 1);
    assert!(core_inputs(&system).is_empty());
    let after = system.execution_context(&actor, &message).unwrap();
    assert!(after.sandbox_policy.is_path_writable(&file));
    assert!(
        !after
            .sandbox_policy
            .is_path_writable(&directory.path().join("unrequested.txt"))
    );
    system.shutdown().await;
}

#[tokio::test]
async fn suspended_read_refusal_stays_local_until_explicit_application_and_is_reviewed_once() {
    let reviewer = Arc::new(Reviewer::new(ReviewReply::Json(
        json!({"decision":"deny","reason":"Owner authorization is needed"}),
    )));
    let (directory, system, actor, message) = fixture(reviewer.clone()).await;
    let secret = directory.path().join("outside.txt");
    std::fs::write(&secret, "private source").unwrap();
    let (call, _) = source(&system, &actor, &message, false);
    let control = ActorControl::default();
    let mut context = system.execution_context(&actor, &message).unwrap();
    runtime::bind_access_broker(&mut context, &system, &actor, &message, &call, &control);
    let filesystem = crate::sandbox::access::FileSystem::new(&context);
    let first = filesystem.read_bytes(&secret, 1024).await.unwrap_err();
    let refusal = rejection(&first).expect("sandbox review must return an immutable refusal");
    assert!(core_inputs(&system).is_empty());
    let repeated = filesystem.read_bytes(&secret, 1024).await.unwrap_err();
    assert_eq!(
        rejection(&repeated).unwrap().approval_id,
        refusal.approval_id
    );
    assert_eq!(reviewer.requests.lock().len(), 1);
    assert!(core_inputs(&system).is_empty());
    let submitted = runtime::submit(
        &system,
        &actor,
        &message,
        &control,
        &context,
        ApplicationSource::Approval {
            approval_id: refusal.approval_id.clone(),
        },
        application(),
    )
    .await
    .unwrap();
    assert!(submitted.appeal.is_some());
    assert_eq!(core_inputs(&system).len(), 1);
    assert_eq!(reviewer.requests.lock().len(), 1);
    assert!(
        !system
            .execution_context(&actor, &message)
            .unwrap()
            .sandbox_policy
            .is_path_readable(&secret)
    );
    system.shutdown().await;
}

#[tokio::test]
async fn failed_and_invalid_reviews_deny_and_forward_instead_of_remembering_scope() {
    for reply in [
        ReviewReply::Fail,
        ReviewReply::Timeout,
        ReviewReply::Json(json!({"decision":"approve"})),
        ReviewReply::Json(json!({"decision":"other","reason":"invalid"})),
    ] {
        let reviewer = Arc::new(Reviewer::new(reply));
        let (directory, system, actor, message) = fixture(reviewer.clone()).await;
        let outside = directory.path().join("source");
        std::fs::create_dir_all(&outside).unwrap();
        let (_, result) = source(&system, &actor, &message, false);
        let base = system.execution_context(&actor, &message).unwrap();
        let record = runtime::submit(
            &system,
            &actor,
            &message,
            &ActorControl::default(),
            &base,
            scope_request(&result, &outside, true),
            application(),
        )
        .await
        .unwrap();
        assert_eq!(record.decision, ApprovalDecision::Denied);
        assert!(record.appeal.is_some());
        assert_eq!(core_inputs(&system).len(), 1);
        assert!(
            !system
                .execution_context(&actor, &message)
                .unwrap()
                .sandbox_policy
                .is_path_writable(&outside)
        );
        assert_eq!(reviewer.requests.lock().len(), 1);
        system.shutdown().await;
    }
}

#[tokio::test]
async fn cancellation_drops_the_blocked_review_and_cannot_grant_or_forward() {
    let reviewer = Arc::new(Reviewer::new(ReviewReply::Pending));
    let (directory, system, actor, message) = fixture(reviewer.clone()).await;
    let outside = directory.path().join("source");
    std::fs::create_dir_all(&outside).unwrap();
    let (_, result) = source(&system, &actor, &message, false);
    let base = system.execution_context(&actor, &message).unwrap();
    let control = ActorControl::default();
    let notified = reviewer.started.notified();
    tokio::pin!(notified);
    notified.as_mut().enable();
    let future = runtime::submit(
        &system,
        &actor,
        &message,
        &control,
        &base,
        scope_request(&result, &outside, true),
        application(),
    );
    tokio::pin!(future);
    tokio::select! {
        _ = &mut notified => {},
        value = &mut future => panic!("pending review returned before cancellation: {value:?}"),
    }
    control.stop.store(true, Ordering::Release);
    assert!(future.await.is_err());
    assert!(reviewer.dropped.load(Ordering::Acquire));
    assert!(core_inputs(&system).is_empty());
    assert!(
        !system
            .execution_context(&actor, &message)
            .unwrap()
            .sandbox_policy
            .is_path_writable(&outside)
    );
    assert_eq!(
        system
            .store()
            .approvals(Some(&actor.id), None, 10)
            .unwrap()
            .items[0]
            .decision,
        ApprovalDecision::Cancelled
    );
    system.shutdown().await;
}

#[tokio::test]
async fn readonly_execution_provenance_and_another_inputs_result_cannot_obtain_scope() {
    let reviewer = Arc::new(Reviewer::new(ReviewReply::Pending));
    let (directory, system, actor, message) = fixture(reviewer.clone()).await;
    let outside = directory.path().join("source");
    std::fs::create_dir_all(&outside).unwrap();
    let (_, result) = source(&system, &actor, &message, true);
    let base = system.execution_context(&actor, &message).unwrap();
    assert!(
        runtime::submit(
            &system,
            &actor,
            &message,
            &ActorControl::default(),
            &base,
            scope_request(&result, &outside, true),
            application()
        )
        .await
        .is_err()
    );
    system.seed_test_actor("other");
    let other = system.actor("other").unwrap();
    let other_input = input(&system, &other.id);
    let other_base = system.execution_context(&other, &other_input).unwrap();
    assert!(
        runtime::submit(
            &system,
            &other,
            &other_input,
            &ActorControl::default(),
            &other_base,
            scope_request(&result, &outside, false),
            application()
        )
        .await
        .is_err()
    );
    assert!(reviewer.requests.lock().is_empty());
    assert!(core_inputs(&system).is_empty());
    system.shutdown().await;
}

#[tokio::test]
async fn core_consent_records_scope_and_one_notification_without_replaying_the_original_effect() {
    let reviewer = Arc::new(Reviewer::new(ReviewReply::Json(
        json!({"decision":"deny","reason":"ask Owner"}),
    )));
    let (directory, system, actor, message) = fixture(reviewer.clone()).await;
    let outside = directory.path().join("source");
    std::fs::create_dir_all(&outside).unwrap();
    let marker = outside.join("effects.txt");
    std::fs::write(&marker, "one original effect").unwrap();
    let (_, result) = source(&system, &actor, &message, false);
    let base = system.execution_context(&actor, &message).unwrap();
    let record = runtime::submit(
        &system,
        &actor,
        &message,
        &ActorControl::default(),
        &base,
        scope_request(&result, &outside, true),
        application(),
    )
    .await
    .unwrap();
    let core_input = system.store().claim_next("core").unwrap().unwrap();
    let core = system.core_device_actor().unwrap();
    let context = system.execution_context(&core, &core_input).unwrap();
    let caller = context.actor_call.unwrap();
    let resolved = system
        .resolve_appeal_from_core(
            &caller,
            &record.id,
            ApprovalDecision::Approved,
            "The Owner authorized this directory in the account conversation",
        )
        .await
        .unwrap();
    assert_eq!(resolved.decision, ApprovalDecision::Denied);
    let appeal = resolved.appeal.unwrap();
    assert_eq!(appeal.decision, ApprovalDecision::Approved);
    assert!(appeal.notification_id.is_some());
    let again = system
        .resolve_appeal_from_core(
            &caller,
            &record.id,
            ApprovalDecision::Approved,
            "The Owner authorized this directory in the account conversation",
        )
        .await
        .unwrap();
    assert_eq!(
        again.appeal.unwrap().notification_id,
        appeal.notification_id
    );
    assert!(
        system
            .execution_context(&actor, &message)
            .unwrap()
            .sandbox_policy
            .is_path_writable(&outside.join("future/report.txt"))
    );
    assert_eq!(reviewer.requests.lock().len(), 1);
    assert_eq!(
        std::fs::read_to_string(marker).unwrap(),
        "one original effect"
    );
    system.shutdown().await;
}

#[test]
fn review_failure_categories_remain_distinguishable() {
    assert!(
        ReviewFailure::Request
            .denial_reason()
            .contains("review_failure=request")
    );
    assert!(
        ReviewFailure::Timeout
            .denial_reason()
            .contains("review_failure=timeout")
    );
}

#[test]
fn review_contract_is_strict_and_has_no_execution_tool_schemas() {
    let schema = model_schema_for::<ReviewVerdict>();
    crate::schema_utils::validate_model_facing_schema(&schema).unwrap();
    assert_eq!(schema["additionalProperties"], false);
    assert!(
        validate_value_against_schema(
            &json!({"decision":"approve","reason":"authorized"}),
            &schema,
            "verdict"
        )
        .is_ok()
    );
    assert!(
        validate_value_against_schema(
            &json!({"decision":"approve","reason":"authorized","tool":"exec"}),
            &schema,
            "verdict"
        )
        .is_err()
    );
    assert!(
        validate_value_against_schema(
            &json!({"decision":"override_grants","reason":"x"}),
            &schema,
            "verdict"
        )
        .is_err()
    );
}
