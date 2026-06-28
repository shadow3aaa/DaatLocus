use std::time::Duration;

use crate::{
    activity_event::{TextActivityDescriptor, ToolCallActivityEvent, compact_preserved_body_lines},
    app::{AppId, AppToolExecutionContext},
    context::{
        ActivePrimitiveRunSession, AppNoticeKey, Context, PendingPrimitiveRunFlush,
        RuntimeTurnPhase,
    },
    context_budget::{
        TokenEstimateBaseline, estimate_agent_turn_request, is_context_budget_exceeded,
    },
    dashboard::render::{
        AUTO_SLEEP_IDLE_THRESHOLD, AUTO_SLEEP_MIN_INTERVAL, FORCE_SLEEP_ERROR_BACKLOG_THRESHOLD,
        render_dashboard_footer_context, sync_dashboard_state,
    },
    dashboard::{
        DashboardActivityEvent, DashboardActivityHistoryStore, DashboardActivityHistoryWindow,
        DashboardControlCommand, DashboardState, SessionActivityEvent,
        activity_event_from_tool_call_activity_event, apply_activity_event,
        assistant_activity_cell, final_message_separator_activity_cell,
        render_activity_from_messages, thinking_activity_cell, user_activity_cell_from_event,
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
            AgentContent, AgentContentPart, AgentMessage, AgentToolCall, AgentTurnItem,
            AgentTurnRequest, AgentTurnStreamResult, HistoryMessage,
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
    workflow::{PrimitiveRunRecord, append_primitive_run_records},
    workspace_app::{WorkspaceAppInvalidation, WorkspaceAppRegistry},
};
use chrono::Utc;
use miette::{Result, miette};
use serde_json::json;

use crate::runtime::bootstrap::{
    build_eval_context_with_compiled, load_compiled_prompts_only, summarize_sleep_summary,
};
mod claimed_input;
mod coding_source_elision;
mod dashboard_control;
mod live_draft;
mod model_driver;
mod scheduler;
mod sleep_driver;
mod turn;
mod workflow_evidence;
mod workspace_apps;

pub(crate) use dashboard_control::handle_dashboard_control_command;
pub(crate) use scheduler::{
    daat_locus_loop, interrupt_active_runtime_turn, reset_cancelled_runtime_turn,
};
pub(crate) use sleep_driver::{SleepTaskResult, handle_sleep_task_result};
pub(crate) use turn::execute_agent_loop_step;
pub(crate) use workflow_evidence::{AgentLoopStepExecution, AgentLoopStepOutput};

use claimed_input::*;
use live_draft::{TelegramLiveDraftSession, maybe_start_telegram_live_draft_session};
use workflow_evidence::{record_runtime_history_messages, record_workflow_run_evidence};
use workspace_apps::{drain_workspace_app_invalidations, sync_workspace_apps_from_invalidation};

const RUNTIME_EVENT_CLAIM_BATCH_SIZE: usize = 1;
const RUNTIME_OVERFLOW_FUSE_THRESHOLD: usize = 3;
const RUNTIME_MODEL_REQUEST_FUSE_THRESHOLD: usize = 3;
const APP_NOTICE_UNRESOLVED_SUPPRESSION_THRESHOLD: usize = 3;
const APP_NOTICE_OVERFLOW_SUPPRESSION: Duration = Duration::from_secs(300);
const RUNTIME_HISTORY_MIN_MESSAGES: usize = 0;
const RUNTIME_HISTORY_SUMMARY_MAX_TOKENS: usize = 800;
const RUNTIME_PREFLIGHT_STAGE_TIMEOUT_SECS: u64 = 60;

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::HashMap, sync::Arc, time::Instant};

    use async_trait::async_trait;
    use miette::{Result, miette};
    use tempfile::TempDir;

    use crate::{
        app::{App, AppManager},
        config::Config,
        context_budget::TokenEstimateBaseline,
        core::Llm,
        memory::Memory,
        openskills::OpenSkillsCatalog,
        plan::Plan,
        reasoning::{compiled::CompiledPromptStore, runtime::PromptRequest},
        runtime::bootstrap::DaatLocusHomeOverride,
        sandbox::RuntimeSandboxPolicy,
        telegram_acl::TelegramAclHandle,
        telegram_transport::state::TelegramTransportState,
        workflow::PrimitiveStore,
        workspace_app::WorkspaceAppRegistry,
    };

    struct UnusedLlm;

    #[async_trait]
    impl Llm for UnusedLlm {
        async fn run_json(
            &self,
            _context: &Context,
            _request: PromptRequest,
        ) -> Result<serde_json::Value> {
            Err(miette!("unused test llm"))
        }

        async fn run_agent_turn(
            &self,
            _context: &Context,
            _request: AgentTurnRequest,
        ) -> Result<AgentTurnStreamResult> {
            Err(miette!("unused test llm"))
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
            let apps = AppManager::new(None, Vec::<Box<dyn App>>::new())
                .await
                .expect("app manager");
            let context = Context {
                session_id: None,
                llm: Box::new(UnusedLlm),
                judge_llm: Box::new(UnusedLlm),
                efficient_llm: Box::new(UnusedLlm),
                config: Config::default(),
                memory: Memory::new().await,
                plan: Plan::new().await,
                events: crate::events::EventStore::new().await,
                pending_work: crate::pending_work::PendingWorkQueue::new().await,
                workflows: PrimitiveStore::new().await,
                openskills: OpenSkillsCatalog::default(),
                bound_primitive_id: None,
                bound_primitive_composition: None,
                active_primitive_run: None,
                pending_primitive_run_flushes: Vec::new(),
                current_work_origin: None,
                workflow_step_started_bound_id: None,
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
                active_app_notices: HashMap::new(),
                runtime_overflow_failures: Arc::new(parking_lot::Mutex::new(HashMap::new())),
                runtime_model_request_failures: Arc::new(parking_lot::Mutex::new(HashMap::new())),
                suppressed_app_notices: Arc::new(parking_lot::Mutex::new(HashMap::new())),
                live_progress_tx: Arc::new(parking_lot::Mutex::new(None)),
                telegram_live_drafts: Arc::new(parking_lot::Mutex::new(HashMap::new())),
                claimed_event_ids: Vec::new(),
                claimed_app_notices: Vec::new(),
                afterclaim_context_fingerprint: None,
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

        let outcome = interrupt_active_runtime_turn(context, "test interrupt");

        assert_eq!(outcome.failed_events, 1);
        assert_eq!(outcome.suppressed_app_notices, 0);
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
    }

    #[tokio::test]
    async fn user_interrupt_suppresses_claimed_app_notice_without_requeueing() {
        let mut isolated = IsolatedRuntimeContext::new().await;
        let context = &mut isolated.context;
        let notice = AppNoticeKey::new(AppId::terminal(), "busy");
        context.activate_app_notice(notice.app.clone(), notice.reason.clone());
        context
            .pending_work
            .enqueue(PendingWork::AppNotice {
                app: notice.app.clone(),
                reason: notice.reason.clone(),
            })
            .expect("enqueue notice");

        let claimed = context.pending_work.claim_batch(1).expect("claim notice");
        assert_eq!(claimed.len(), 1);
        context.claimed_app_notices = vec![notice.clone()];
        context.active_runtime_turn = true;
        context.runtime_turn_started_at = Some(Instant::now());
        context.runtime_turn_started_at_ms = Some(42);

        let outcome = interrupt_active_runtime_turn(context, "test interrupt");

        assert_eq!(outcome.failed_events, 0);
        assert_eq!(outcome.suppressed_app_notices, 1);
        assert!(!context.active_runtime_turn);
        assert!(context.claimed_app_notices.is_empty());
        assert_eq!(context.pending_work.pending_count(), 0);
        assert!(
            context
                .pending_work
                .claim_batch(1)
                .expect("claim after interrupt")
                .is_empty()
        );
        assert!(context.is_app_notice_suppressed(&notice.app, &notice.reason));
        assert!(!context.active_app_notices.contains_key(&notice));
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
    fn runtime_turn_follow_up_decision_state_machine_prefers_runtime_gate() {
        let state = RuntimeTurnFollowUpState {
            raw_stream_requested_follow_up: true,
            claimed_statuses: &[],
            has_claimed_app_notice: false,
            claimed_app_notice_resolved: false,
        };
        assert!(matches!(
            runtime_turn_follow_up_decision_from_state(&state),
            RuntimeFollowUpDecision::Continue { .. }
        ));

        let state = RuntimeTurnFollowUpState {
            raw_stream_requested_follow_up: false,
            claimed_statuses: &[EventStatus::Claimed],
            has_claimed_app_notice: false,
            claimed_app_notice_resolved: false,
        };
        assert!(matches!(
            runtime_turn_follow_up_decision_from_state(&state),
            RuntimeFollowUpDecision::Continue { .. }
        ));

        let state = RuntimeTurnFollowUpState {
            raw_stream_requested_follow_up: false,
            claimed_statuses: &[EventStatus::Resolved],
            has_claimed_app_notice: false,
            claimed_app_notice_resolved: false,
        };
        assert!(matches!(
            runtime_turn_follow_up_decision_from_state(&state),
            RuntimeFollowUpDecision::AllowFinish
        ));

        let state = RuntimeTurnFollowUpState {
            raw_stream_requested_follow_up: false,
            claimed_statuses: &[EventStatus::Resolved],
            has_claimed_app_notice: true,
            claimed_app_notice_resolved: false,
        };
        assert!(matches!(
            runtime_turn_follow_up_decision_from_state(&state),
            RuntimeFollowUpDecision::Continue { .. }
        ));

        let state = RuntimeTurnFollowUpState {
            raw_stream_requested_follow_up: false,
            claimed_statuses: &[EventStatus::Resolved],
            has_claimed_app_notice: true,
            claimed_app_notice_resolved: true,
        };
        assert!(matches!(
            runtime_turn_follow_up_decision_from_state(&state),
            RuntimeFollowUpDecision::AllowFinish
        ));
    }

    #[test]
    fn claimed_runtime_input_fingerprint_is_stable_and_sorted() {
        let event_a = uuid::Uuid::parse_str("00000000-0000-0000-0000-000000000002").unwrap();
        let event_b = uuid::Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();
        let inputs = vec![
            ClaimedRuntimeInput::AppNotice {
                app: AppId::terminal(),
                reason: "busy".to_string(),
            },
            ClaimedRuntimeInput::Event(Box::new(EventView {
                event_id: event_a,
                source: crate::events::EventSource::Telegram,
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
            })),
            ClaimedRuntimeInput::Event(Box::new(EventView {
                event_id: event_b,
                source: crate::events::EventSource::Telegram,
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
            })),
        ];

        assert_eq!(
            claimed_runtime_input_fingerprint(&inputs).as_deref(),
            Some(
                "events=[00000000-0000-0000-0000-000000000001,00000000-0000-0000-0000-000000000002]|app_notices=[terminal:busy]"
            )
        );
    }

    #[test]
    fn claimed_runtime_input_fingerprint_is_none_for_empty_batch() {
        assert_eq!(claimed_runtime_input_fingerprint(&[]), None);
    }

    #[test]
    fn follow_up_reason_messages_are_structured() {
        assert_eq!(
            RuntimeFollowUpReason::RawStreamRequestedFollowUp.message(),
            "This sample is still marked needs_follow_up; continue the current turn."
        );
        assert!(
            RuntimeFollowUpReason::ClaimedEventNeedsExplicitResolution
                .message()
                .contains("finish_and_send")
        );
        assert!(
            RuntimeFollowUpReason::ClaimedAppNoticeNeedsExplicitResolution
                .message()
                .contains("notice_resolved")
        );
    }

    #[test]
    fn overflow_failure_note_includes_attempt_count_and_error() {
        assert_eq!(
            runtime_overflow_failure_note(3, "context limit exceeded"),
            "runtime context overflow persisted after 3 attempts: context limit exceeded"
        );
    }
}
