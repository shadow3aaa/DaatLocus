#[cfg(test)]
use std::collections::HashSet;
use std::time::Duration;

use crate::{
    activity_event::{TextActivityDescriptor, ToolCallActivityEvent, compact_preserved_body_lines},
    app::{AppId, AppToolExecutionContext},
    context::{Context, RuntimeTurnPhase},
    context_budget::{TokenEstimateBaseline, is_context_budget_exceeded},
    dashboard::render::{
        AUTO_SLEEP_IDLE_THRESHOLD, AUTO_SLEEP_MIN_INTERVAL, FORCE_SLEEP_ERROR_BACKLOG_THRESHOLD,
        render_dashboard_footer_context, sync_dashboard_state,
    },
    dashboard::{
        DashboardActivityEvent, DashboardActivityHistoryStore, DashboardActivityHistoryWindow,
        DashboardControlCommand, DashboardState, SessionActivityEvent,
        activity_event_from_tool_call_activity_event, apply_activity_event,
        assistant_activity_cell, render_activity_from_messages, thinking_activity_cell,
        user_activity_cell_from_event,
    },
    events::{EventPayload, EventStatus, EventView},
    logging::{
        RuntimeStatusLevel, clear_runtime_status, set_runtime_status, set_runtime_status_only,
        write_current_turn_messages_dump, write_current_turn_response_dump,
        write_current_turn_response_error_dump,
    },
    memory::RuntimeTurnDraft,
    pending_work::PendingWork,
    preturn_state::PreTurnState,
    reasoning::{
        episode::EpisodeActionRecord,
        prompt_parts::AfterClaimContextInput,
        runtime::{
            AgentContent, AgentContentPart, AgentMessage, AgentToolCall, AgentTurnRequest,
            AgentTurnStreamResult, HistoryMessage,
        },
        runtime_error::{
            RuntimeErrorActionContext, RuntimeErrorCase, RuntimeErrorCaseParts, RuntimeErrorKind,
            RuntimeErrorObservation, RuntimeErrorRuntimeContext, RuntimeErrorTaskContext,
            append_runtime_error_case,
        },
        sleep::run_sleep,
    },
    runtime_context::{
        MID_TURN_COMPACTION_MAX_RECOVERIES, build_afterclaim_context_text,
        build_preturn_context_text, build_runtime_request_envelope,
        execute_pre_turn_runtime_compaction, maybe_compact_runtime_messages,
        runtime_request_budget_limits,
    },
    runtime_tools::{
        ToolExecutionResult, build_runtime_tool_specs, build_tool_call_activity_event,
        execute_agent_tool_call, render_telegram_tool_result_status,
        summarize_action_from_tool_call,
    },
    sleep_status::{
        SleepStatusSnapshot, persist_sleep_status_snapshot, refresh_sleep_status_queues,
    },
};
use miette::{Result, miette};
use serde_json::json;

use crate::runtime::bootstrap::{
    build_eval_context_with_compiled, load_compiled_prompts_only, summarize_sleep_summary,
};
mod claimed_input;
pub mod coding_source_elision;
mod dashboard_control;
mod live_draft;
mod model_driver;
mod scheduler;
mod sleep_driver;
mod turn;
mod workflow_evidence;
mod workspace_apps;

pub use dashboard_control::handle_dashboard_control_command;
pub use scheduler::{
    RuntimeLoopCycle, daat_locus_loop, interrupt_active_runtime_turn, reset_cancelled_runtime_turn,
};
pub use sleep_driver::{SleepTaskResult, handle_sleep_task_result};
pub use turn::{append_workflow_activity_event, execute_agent_loop_step};
pub use workflow_evidence::{AgentLoopStepExecution, AgentLoopStepOutput};

use claimed_input::{
    ClaimedRuntimeInput, afterclaim_context_input_for_claimed_inputs, claim_pending_runtime_inputs,
    claimed_events_are_terminal, claimed_events_require_explicit_completion,
    claimed_runtime_input_fingerprint, finalize_claimed_runtime_events,
    handle_model_request_failure, handle_runtime_overflow, requeue_claimed_runtime_events,
    runtime_work_origin,
};
use live_draft::{TelegramLiveDraftSession, maybe_start_telegram_live_draft_session};
use workflow_evidence::{
    maybe_record_skill_read, record_runtime_history_messages, record_skill_run_evidence,
};
use workspace_apps::sync_workspace_apps_from_invalidation;

const RUNTIME_EVENT_CLAIM_BATCH_SIZE: usize = 1;
const RUNTIME_OVERFLOW_FUSE_THRESHOLD: usize = 3;
const RUNTIME_MODEL_REQUEST_FUSE_THRESHOLD: usize = 3;
const RUNTIME_HISTORY_MIN_MESSAGES: usize = 0;
const RUNTIME_HISTORY_SUMMARY_MAX_TOKENS: usize = 800;
const RUNTIME_PREFLIGHT_STAGE_TIMEOUT_SECS: u64 = 60;

#[cfg(test)]
mod tests {
    use super::claimed_input::{
        ClaimedEventStatusSummary, claimed_event_statuses_are_terminal,
        runtime_overflow_failure_note, summarize_claimed_event_statuses,
    };
    use super::turn::{
        clear_runtime_failures_after_model_success, clear_runtime_overflow_failure_after_compaction,
    };
    use super::*;
    use std::{collections::HashMap, sync::Arc, time::Instant};

    use async_trait::async_trait;
    use miette::{Result, miette};
    use tempfile::TempDir;

    use crate::{
        app::{App, AppManager},
        config::Config,
        context_budget::TokenEstimateBaseline,
        core::{ModelProvider, ModelRequestOptions},
        memory::Memory,
        openskills::OpenSkillsCatalog,
        plan::Plan,
        reasoning::{
            compiled::CompiledPromptStore,
            runtime::{AgentTurnItem, PromptRequest},
        },
        runtime::bootstrap::DaatLocusHomeOverride,
        sandbox::RuntimeSandboxPolicy,
        telegram_acl::TelegramAclHandle,
        telegram_transport::state::TelegramTransportState,
        workspace_app::WorkspaceAppRegistry,
    };

    struct UnusedModelProvider;

    struct TextCompactionModelProvider {
        summary: &'static str,
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    struct OverflowRecoveryModelProvider {
        agent_requests: Arc<std::sync::Mutex<Vec<AgentTurnRequest>>>,
        compaction_calls: Arc<std::sync::atomic::AtomicUsize>,
        succeed_on_agent_request: Option<usize>,
    }

    struct InterruptCheckpointModelProvider {
        calls: std::sync::atomic::AtomicUsize,
        second_request_started: Arc<tokio::sync::Notify>,
    }

    #[async_trait]
    impl ModelProvider for TextCompactionModelProvider {
        async fn complete_json(
            &self,
            _request: PromptRequest,
            _options: ModelRequestOptions,
        ) -> Result<serde_json::Value> {
            Err(miette!("compaction must not use a structured tool request"))
        }

        async fn complete_agent_turn(
            &self,
            request: AgentTurnRequest,
            _options: ModelRequestOptions,
        ) -> Result<AgentTurnStreamResult> {
            assert!(request.tools.is_empty());
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(AgentTurnStreamResult {
                items: Vec::new(),
                raw_stream_follow_up: false,
                last_assistant_message: Some(self.summary.to_string()),
                last_reasoning_content: None,
            })
        }

        fn request_budget_limits(&self) -> crate::context_budget::RequestBudgetLimits {
            crate::context_budget::RequestBudgetLimits {
                context_window_tokens: crate::context_budget::DEFAULT_CONTEXT_WINDOW_TOKENS,
                auto_compact_threshold_tokens: crate::context_budget::DEFAULT_CONTEXT_WINDOW_TOKENS,
                reserved_output_tokens: crate::context_budget::DEFAULT_MAX_COMPLETION_TOKENS,
            }
        }

        fn token_usage_info(&self) -> crate::core::TokenUsageInfo {
            crate::core::TokenUsageInfo::default()
        }

        fn model_name(&self) -> String {
            "text-compaction-test".to_string()
        }
    }

    #[async_trait]
    impl ModelProvider for OverflowRecoveryModelProvider {
        async fn complete_json(
            &self,
            _request: PromptRequest,
            _options: ModelRequestOptions,
        ) -> Result<serde_json::Value> {
            Err(miette!(
                "overflow recovery must use a text compaction request"
            ))
        }

        async fn complete_agent_turn(
            &self,
            request: AgentTurnRequest,
            options: ModelRequestOptions,
        ) -> Result<AgentTurnStreamResult> {
            if request.tools.is_empty() {
                self.compaction_calls
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                return Ok(AgentTurnStreamResult {
                    items: Vec::new(),
                    raw_stream_follow_up: false,
                    last_assistant_message: Some("overflow recovery summary".to_string()),
                    last_reasoning_content: None,
                });
            }

            let request_count = {
                let mut requests = self.agent_requests.lock().expect("agent requests lock");
                requests.push(request);
                requests.len()
            };
            if self.succeed_on_agent_request != Some(request_count) {
                return Err(
                    crate::context_budget::ContextBudgetExceededError::for_request(
                        "test agent turn",
                        &self.model_name(),
                        &options.budget,
                        Some("simulated provider overflow"),
                    )
                    .into(),
                );
            }

            Ok(AgentTurnStreamResult {
                items: Vec::new(),
                raw_stream_follow_up: false,
                last_assistant_message: Some("recovered".to_string()),
                last_reasoning_content: None,
            })
        }

        fn request_budget_limits(&self) -> crate::context_budget::RequestBudgetLimits {
            crate::context_budget::RequestBudgetLimits {
                context_window_tokens: 1_000_000,
                auto_compact_threshold_tokens: 900_000,
                reserved_output_tokens: 50_000,
            }
        }

        fn token_usage_info(&self) -> crate::core::TokenUsageInfo {
            crate::core::TokenUsageInfo::default()
        }

        fn model_name(&self) -> String {
            "overflow-recovery-test".to_string()
        }
    }

    #[async_trait]
    impl ModelProvider for InterruptCheckpointModelProvider {
        async fn complete_json(
            &self,
            _request: PromptRequest,
            _options: ModelRequestOptions,
        ) -> Result<serde_json::Value> {
            Err(miette!("checkpoint test does not use structured output"))
        }

        async fn complete_agent_turn(
            &self,
            _request: AgentTurnRequest,
            _options: ModelRequestOptions,
        ) -> Result<AgentTurnStreamResult> {
            let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if call == 0 {
                return Ok(AgentTurnStreamResult {
                    items: vec![AgentTurnItem::ToolCall {
                        call: AgentToolCall {
                            id: "checkpoint-plan".to_string(),
                            name: "update_plan".to_string(),
                            arguments: json!({
                                "explanation": "Preserve completed work before interruption.",
                                "plan": [{
                                    "step": "Checkpoint completed turn messages",
                                    "status": "in_progress"
                                }]
                            }),
                        },
                    }],
                    raw_stream_follow_up: false,
                    last_assistant_message: None,
                    last_reasoning_content: None,
                });
            }

            self.second_request_started.notify_one();
            std::future::pending().await
        }

        fn request_budget_limits(&self) -> crate::context_budget::RequestBudgetLimits {
            crate::context_budget::RequestBudgetLimits {
                context_window_tokens: crate::context_budget::DEFAULT_CONTEXT_WINDOW_TOKENS,
                auto_compact_threshold_tokens: crate::context_budget::DEFAULT_CONTEXT_WINDOW_TOKENS,
                reserved_output_tokens: crate::context_budget::DEFAULT_MAX_COMPLETION_TOKENS,
            }
        }

        fn token_usage_info(&self) -> crate::core::TokenUsageInfo {
            crate::core::TokenUsageInfo::default()
        }

        fn model_name(&self) -> String {
            "interrupt-checkpoint-test".to_string()
        }
    }

    #[async_trait]
    impl ModelProvider for UnusedModelProvider {
        async fn complete_json(
            &self,
            _request: PromptRequest,
            _options: ModelRequestOptions,
        ) -> Result<serde_json::Value> {
            Err(miette!("unused test model provider"))
        }

        async fn complete_agent_turn(
            &self,
            _request: AgentTurnRequest,
            _options: ModelRequestOptions,
        ) -> Result<AgentTurnStreamResult> {
            Err(miette!("unused test model provider"))
        }

        fn request_budget_limits(&self) -> crate::context_budget::RequestBudgetLimits {
            crate::context_budget::RequestBudgetLimits {
                context_window_tokens: crate::context_budget::DEFAULT_CONTEXT_WINDOW_TOKENS,
                auto_compact_threshold_tokens: crate::context_budget::DEFAULT_CONTEXT_WINDOW_TOKENS,
                reserved_output_tokens: crate::context_budget::DEFAULT_MAX_COMPLETION_TOKENS,
            }
        }

        fn token_usage_info(&self) -> crate::core::TokenUsageInfo {
            crate::core::TokenUsageInfo::default()
        }

        fn model_name(&self) -> String {
            "unused-test-model-provider".to_string()
        }
    }

    struct IsolatedRuntimeContext {
        context: Context,
        _home_override: DaatLocusHomeOverride,
        _home: TempDir,
        _execution: TempDir,
    }

    impl IsolatedRuntimeContext {
        async fn new() -> Self {
            let home = tempfile::tempdir().expect("test home");
            let execution = tempfile::tempdir().expect("test execution cwd");
            let home_override = DaatLocusHomeOverride::set(home.path().to_path_buf()).await;
            let telegram = TelegramTransportState::new();
            let (daemon_control_tx, _daemon_control_rx) = tokio::sync::mpsc::unbounded_channel();
            let apps = AppManager::new(Vec::<Box<dyn App>>::new()).expect("app manager");
            let context = Context {
                session_id: None,
                model_provider: Box::new(UnusedModelProvider),
                efficient_model_provider: std::sync::Arc::new(UnusedModelProvider),
                config: Config::default(),
                memory: Memory::new().await,
                plan: Plan::new().await,
                events: crate::events::EventStore::new().await,
                pending_work: crate::pending_work::PendingWorkQueue::new().await,
                openskills: OpenSkillsCatalog::default(),
                workflows: crate::workflow::WorkflowCatalog::load(),
                workflow_cancellation: crate::workflow::WorkflowCancellationRegistry::default(),
                active_skill_run: None,
                pending_skill_run_flushes: Vec::new(),
                current_work_origin: None,
                apps,
                workspace_apps: WorkspaceAppRegistry::default(),
                telegram: telegram.handle(),
                telegram_acl: TelegramAclHandle::load().await,
                compiled_prompts: CompiledPromptStore::from_entries(Vec::new()),
                execution_cwd: execution.path().to_path_buf(),
                coding_project_dir: None,
                sandbox_policy: RuntimeSandboxPolicy::disabled(),
                dashboard_tx: None,
                dashboard_history: None,
                daemon_control_tx,
                latest_context_composition: None,
                active_runtime_turn: false,
                active_runtime_phase: None,
                runtime_turn_started_at: None,
                runtime_turn_started_at_ms: None,
                runtime_turn_epoch: 0,
                runtime_overflow_failures: Arc::new(parking_lot::Mutex::new(HashMap::new())),
                runtime_model_request_failures: Arc::new(parking_lot::Mutex::new(HashMap::new())),
                live_progress_tx: Arc::new(parking_lot::Mutex::new(None)),
                telegram_live_drafts: Arc::new(parking_lot::Mutex::new(HashMap::new())),
                claimed_event_ids: Vec::new(),
                afterclaim_context_fingerprint: None,
                visible_source_lines: HashSet::new(),
                delivered_root_instruction_fingerprint: None,
                idle_since: None,
                last_idle_sleep_at: None,
                session_title: crate::runtime::session_title::SessionTitleState::default(),
                token_estimate_baseline: TokenEstimateBaseline::default(),
            };
            Self {
                context,
                _home_override: home_override,
                _home: home,
                _execution: execution,
            }
        }
    }

    #[tokio::test]
    async fn pre_turn_compaction_uses_main_model_without_calling_efficient_model() {
        let mut isolated = IsolatedRuntimeContext::new().await;
        let context = &mut isolated.context;
        let main_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        context.model_provider = Box::new(TextCompactionModelProvider {
            summary: "## Current state\n\nmain model summary",
            calls: main_calls.clone(),
        });
        context.efficient_model_provider = Arc::new(UnusedModelProvider);

        let main_model = context.config.main_model.clone();
        let model = context
            .config
            .models
            .get_mut(&main_model)
            .expect("default main model");
        model.context_window_tokens = 1_000_000;
        model.effective_context_window_percent = 100;
        model.max_completion_tokens = 1;
        let plan = crate::memory::RuntimeConversationCompactionPlan::for_test(
            vec![HistoryMessage::user("user input")],
            1_024,
        );

        let outcome = crate::runtime_context::execute_pre_turn_runtime_compaction(context, &plan)
            .await
            .expect("main model compaction should succeed");

        assert_eq!(main_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(outcome.summary.contains("main model summary"));
        drop(isolated);
    }

    #[tokio::test]
    async fn overflow_recovery_retries_with_compacted_messages_in_the_same_step() {
        let mut isolated = IsolatedRuntimeContext::new().await;
        let context = &mut isolated.context;
        let agent_requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let compaction_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        context.model_provider = Box::new(OverflowRecoveryModelProvider {
            agent_requests: agent_requests.clone(),
            compaction_calls: compaction_calls.clone(),
            succeed_on_agent_request: Some(2),
        });

        execute_agent_loop_step(context, None).await;

        assert_eq!(
            compaction_calls.load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        let requests = agent_requests.lock().expect("agent requests lock");
        assert_eq!(requests.len(), 2);
        assert!(requests[1].messages.iter().any(|message| {
            matches!(message, AgentMessage::Assistant { content } if content.contains("overflow recovery summary"))
        }));
        drop(requests);
        drop(isolated);
    }

    #[tokio::test]
    async fn overflow_recovery_stops_after_three_compactions() {
        let mut isolated = IsolatedRuntimeContext::new().await;
        let context = &mut isolated.context;
        let agent_requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let compaction_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        context.model_provider = Box::new(OverflowRecoveryModelProvider {
            agent_requests: agent_requests.clone(),
            compaction_calls: compaction_calls.clone(),
            succeed_on_agent_request: None,
        });

        execute_agent_loop_step(context, None).await;

        assert_eq!(
            compaction_calls.load(std::sync::atomic::Ordering::SeqCst),
            MID_TURN_COMPACTION_MAX_RECOVERIES
        );
        assert_eq!(
            agent_requests.lock().expect("agent requests lock").len(),
            MID_TURN_COMPACTION_MAX_RECOVERIES + 1
        );
        drop(isolated);
    }
    fn terminal_event(text: &str) -> crate::events::TerminalIncomingEvent {
        crate::events::TerminalIncomingEvent {
            origin: "test".to_string(),
            incoming_text: text.to_string(),
            attachments: Vec::new(),
        }
    }

    #[tokio::test]
    async fn user_interrupt_terminates_claimed_event_without_requeueing() {
        let mut isolated = IsolatedRuntimeContext::new().await;
        let context = &mut isolated.context;
        let event_id = context
            .events
            .register_terminal_incoming(terminal_event("interrupt me"))
            .expect("register event");
        context
            .pending_work
            .enqueue(PendingWork::Event { event_id })
            .expect("enqueue event");

        let claimed = claim_pending_runtime_inputs(context, 1);
        assert_eq!(claimed.len(), 1);
        context.claimed_event_ids = vec![event_id.to_string()];
        context.active_runtime_turn = true;
        context.runtime_turn_started_at = Some(Instant::now());
        context.runtime_turn_started_at_ms = Some(42);

        let failed = interrupt_active_runtime_turn(context, "test interrupt");

        assert_eq!(failed, 1);
        assert!(!context.active_runtime_turn);
        assert!(context.runtime_turn_started_at.is_none());
        assert!(context.claimed_event_ids.is_empty());
        let event = context
            .events
            .view(&event_id.to_string())
            .expect("event view");
        assert_eq!(event.status, EventStatus::Failed);
        assert!(
            event
                .last_error
                .as_deref()
                .is_some_and(|note| note.contains("interrupted by user"))
        );
        assert_eq!(context.pending_work.pending_count(), 0);
        assert!(
            context
                .pending_work
                .claim_batch(1)
                .expect("claim after interrupt")
                .is_empty()
        );
        drop(isolated);
    }

    #[tokio::test]
    async fn interrupted_turn_preserves_completed_tool_protocol_for_next_turn() {
        let mut isolated = IsolatedRuntimeContext::new().await;
        let context = &mut isolated.context;
        let second_request_started = Arc::new(tokio::sync::Notify::new());
        context.model_provider = Box::new(InterruptCheckpointModelProvider {
            calls: std::sync::atomic::AtomicUsize::new(0),
            second_request_started: second_request_started.clone(),
        });
        context.active_runtime_turn = true;
        context.runtime_turn_started_at = Some(Instant::now());
        context.runtime_turn_started_at_ms = Some(42);

        let mut turn = Box::pin(execute_agent_loop_step(context, None));
        tokio::select! {
            () = second_request_started.notified() => {}
            _ = tokio::time::sleep(Duration::from_secs(5)) => {
                panic!("turn did not reach the second model request");
            }
            _ = &mut turn => panic!("turn completed before interruption"),
        }
        drop(turn);

        interrupt_active_runtime_turn(context, "test interrupt after tool result");

        let messages = context.memory.runtime_conversation_messages();
        let tool_call_count = messages
            .iter()
            .filter(|message| {
                matches!(
                    &message.message,
                    AgentMessage::AssistantToolCallProtocol { calls, .. }
                        if calls.iter().any(|call| call.id == "checkpoint-plan")
                )
            })
            .count();
        let tool_result_count = messages
            .iter()
            .filter(|message| {
                matches!(
                    &message.message,
                    AgentMessage::Tool { tool_call_id, .. }
                        if tool_call_id == "checkpoint-plan"
                )
            })
            .count();

        assert_eq!(tool_call_count, 1);
        assert_eq!(tool_result_count, 1);
        drop(isolated);
    }

    #[tokio::test]
    async fn cancelled_turn_reset_still_requeues_claimed_event() {
        let mut isolated = IsolatedRuntimeContext::new().await;
        let context = &mut isolated.context;
        let event_id = context
            .events
            .register_terminal_incoming(terminal_event("recover me"))
            .expect("register event");
        context
            .pending_work
            .enqueue(PendingWork::Event { event_id })
            .expect("enqueue event");

        let claimed = claim_pending_runtime_inputs(context, 1);
        assert_eq!(claimed.len(), 1);
        context.claimed_event_ids = vec![event_id.to_string()];
        context.active_runtime_turn = true;
        context.runtime_turn_started_at = Some(Instant::now());
        context.runtime_turn_started_at_ms = Some(42);

        reset_cancelled_runtime_turn(context, "test stale reset");

        assert!(!context.active_runtime_turn);
        assert!(context.claimed_event_ids.is_empty());
        let event = context
            .events
            .view(&event_id.to_string())
            .expect("event view");
        assert_eq!(event.status, EventStatus::Pending);
        assert_eq!(context.pending_work.pending_count(), 1);
        let reclaimed = context
            .pending_work
            .claim_batch(1)
            .expect("claim after reset");
        assert_eq!(reclaimed.len(), 1);
        assert!(matches!(reclaimed[0], PendingWork::Event { event_id: id } if id == event_id));
        drop(isolated);
    }

    #[test]
    fn claimed_terminal_status_depends_only_on_statuses() {
        assert!(claimed_event_statuses_are_terminal(&[
            EventStatus::AwaitingDelivery
        ]));
        assert!(claimed_event_statuses_are_terminal(&[
            EventStatus::Resolved
        ]));
        assert!(claimed_event_statuses_are_terminal(&[
            EventStatus::Dismissed
        ]));
        assert!(claimed_event_statuses_are_terminal(&[EventStatus::Failed]));
        assert!(!claimed_event_statuses_are_terminal(&[
            EventStatus::Claimed
        ]));
        assert!(claimed_event_statuses_are_terminal(&[
            EventStatus::AwaitingDelivery,
            EventStatus::Resolved,
        ]));
        assert!(claimed_event_statuses_are_terminal(&[
            EventStatus::Resolved,
            EventStatus::Dismissed,
        ]));
        assert!(!claimed_event_statuses_are_terminal(&[
            EventStatus::AwaitingDelivery,
            EventStatus::Claimed,
        ]));
        assert!(!claimed_event_statuses_are_terminal(&[]));
    }

    #[test]
    fn claimed_status_summary_tracks_claimed_and_terminal_reason() {
        assert_eq!(
            summarize_claimed_event_statuses(&[EventStatus::Claimed]),
            ClaimedEventStatusSummary {
                has_claimed: true,
                all_terminal: false,
            }
        );
        assert_eq!(
            summarize_claimed_event_statuses(&[
                EventStatus::AwaitingDelivery,
                EventStatus::Resolved,
            ]),
            ClaimedEventStatusSummary {
                has_claimed: false,
                all_terminal: true,
            }
        );
        assert_eq!(
            summarize_claimed_event_statuses(&[EventStatus::Resolved, EventStatus::Failed,]),
            ClaimedEventStatusSummary {
                has_claimed: false,
                all_terminal: true,
            }
        );
        assert_eq!(
            summarize_claimed_event_statuses(&[EventStatus::Resolved, EventStatus::Claimed,]),
            ClaimedEventStatusSummary {
                has_claimed: true,
                all_terminal: false,
            }
        );
    }

    #[test]
    fn claimed_runtime_input_fingerprint_is_stable_and_sorted() {
        let event_a = uuid::Uuid::parse_str("00000000-0000-0000-0000-000000000002").unwrap();
        let event_b = uuid::Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();
        let inputs = vec![
            EventView {
                event_id: event_a,
                status: EventStatus::Pending,
                reply_message: None,
                arrived_at_ms: 0,
                payload: EventPayload::TelegramIncoming(crate::events::TelegramIncomingEvent {
                    chat_id: "1".to_string(),
                    chat_kind: "private".to_string(),
                    chat_title: "chat".to_string(),
                    sender: "alice".to_string(),
                    incoming_text: "hello".to_string(),
                    telegram_update_id: 1,
                    telegram_message_id: None,
                    telegram_message_date: None,
                    attachments: Vec::new(),
                }),
                last_error: None,
            },
            EventView {
                event_id: event_b,
                status: EventStatus::Pending,
                reply_message: None,
                arrived_at_ms: 0,
                payload: EventPayload::TelegramIncoming(crate::events::TelegramIncomingEvent {
                    chat_id: "2".to_string(),
                    chat_kind: "private".to_string(),
                    chat_title: "chat".to_string(),
                    sender: "bob".to_string(),
                    incoming_text: "world".to_string(),
                    telegram_update_id: 2,
                    telegram_message_id: None,
                    telegram_message_date: None,
                    attachments: Vec::new(),
                }),
                last_error: None,
            },
        ];

        assert_eq!(
            claimed_runtime_input_fingerprint(&inputs).as_deref(),
            Some(
                "events=[00000000-0000-0000-0000-000000000001,00000000-0000-0000-0000-000000000002]"
            )
        );
    }

    #[test]
    fn claimed_runtime_input_fingerprint_is_none_for_empty_batch() {
        assert_eq!(claimed_runtime_input_fingerprint(&[]), None);
    }

    #[tokio::test]
    async fn overflow_fuse_trips_across_requeued_turns() {
        let mut isolated = IsolatedRuntimeContext::new().await;
        let context = &mut isolated.context;
        let event_id = context
            .events
            .register_terminal_incoming(terminal_event("overflow me"))
            .expect("register event");
        context
            .pending_work
            .enqueue(PendingWork::Event { event_id })
            .expect("enqueue event");
        let fingerprint = format!("events=[{event_id}]");
        let event_ids = vec![event_id.to_string()];

        for expected_attempt in 1..RUNTIME_OVERFLOW_FUSE_THRESHOLD {
            let claimed = claim_pending_runtime_inputs(context, 1);
            assert_eq!(claimed.len(), 1);
            assert!(!handle_runtime_overflow(
                context,
                Some(&fingerprint),
                &event_ids,
                "context limit exceeded",
            ));
            assert_eq!(
                *context
                    .runtime_overflow_failures
                    .lock()
                    .get(&fingerprint)
                    .expect("overflow attempt should persist while requeued"),
                expected_attempt
            );
        }

        let claimed = claim_pending_runtime_inputs(context, 1);
        assert_eq!(claimed.len(), 1);
        assert!(handle_runtime_overflow(
            context,
            Some(&fingerprint),
            &event_ids,
            "context limit exceeded",
        ));
        let event = context
            .events
            .view(&event_id.to_string())
            .expect("event view");
        assert_eq!(event.status, EventStatus::Failed);
        assert_eq!(context.pending_work.pending_count(), 0);
        assert!(
            context
                .runtime_overflow_failures
                .lock()
                .get(&fingerprint)
                .is_none()
        );
        drop(isolated);
    }

    #[tokio::test]
    async fn successful_model_request_clears_both_failure_counters() {
        let isolated = IsolatedRuntimeContext::new().await;
        let context = &isolated.context;
        let fingerprint = "events=[test]";
        context.record_runtime_overflow_failure(fingerprint);
        context.record_model_request_failure(fingerprint);

        clear_runtime_failures_after_model_success(context, Some(fingerprint));

        assert_eq!(context.record_runtime_overflow_failure(fingerprint), 1);
        assert_eq!(context.record_model_request_failure(fingerprint), 1);
        drop(isolated);
    }

    #[tokio::test]
    async fn successful_compaction_only_clears_overflow_failure_counter() {
        let isolated = IsolatedRuntimeContext::new().await;
        let context = &isolated.context;
        let fingerprint = "events=[test]";
        context.record_runtime_overflow_failure(fingerprint);
        context.record_model_request_failure(fingerprint);

        clear_runtime_overflow_failure_after_compaction(context, Some(fingerprint));

        assert_eq!(context.record_runtime_overflow_failure(fingerprint), 1);
        assert_eq!(context.record_model_request_failure(fingerprint), 2);
        drop(isolated);
    }

    #[tokio::test]
    async fn model_request_fuse_trips_across_requeued_turns() {
        let mut isolated = IsolatedRuntimeContext::new().await;
        let context = &mut isolated.context;
        let event_id = context
            .events
            .register_terminal_incoming(terminal_event("model failure"))
            .expect("register event");
        context
            .pending_work
            .enqueue(PendingWork::Event { event_id })
            .expect("enqueue event");
        let fingerprint = format!("events=[{event_id}]");
        let event_ids = vec![event_id.to_string()];

        for expected_attempt in 1..RUNTIME_MODEL_REQUEST_FUSE_THRESHOLD {
            let claimed = claim_pending_runtime_inputs(context, 1);
            assert_eq!(claimed.len(), 1);
            assert!(!handle_model_request_failure(
                context,
                Some(&fingerprint),
                &event_ids,
                "temporary provider failure",
                true,
            ));
            assert_eq!(
                *context
                    .runtime_model_request_failures
                    .lock()
                    .get(&fingerprint)
                    .expect("model failure attempt should persist while requeued"),
                expected_attempt
            );
        }

        let claimed = claim_pending_runtime_inputs(context, 1);
        assert_eq!(claimed.len(), 1);
        assert!(handle_model_request_failure(
            context,
            Some(&fingerprint),
            &event_ids,
            "temporary provider failure",
            true,
        ));
        let event = context
            .events
            .view(&event_id.to_string())
            .expect("event view");
        assert_eq!(event.status, EventStatus::Failed);
        assert_eq!(context.pending_work.pending_count(), 0);
        assert!(
            context
                .runtime_model_request_failures
                .lock()
                .get(&fingerprint)
                .is_none()
        );
        drop(isolated);
    }

    #[test]
    fn overflow_failure_note_includes_attempt_count_and_error() {
        assert_eq!(
            runtime_overflow_failure_note(3, "context limit exceeded"),
            "runtime context overflow persisted after 3 attempts: context limit exceeded"
        );
    }
}
