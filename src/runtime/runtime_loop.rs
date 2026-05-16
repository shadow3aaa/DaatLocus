use std::time::Duration;

use crate::{
    app::AppId,
    apply_patch::summarize_apply_patch_error,
    context::{
        ActiveWorkflowRunSession, AppNoticeKey, Context, PendingWorkflowRunFlush, RuntimeTurnPhase,
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
        DashboardControlCommand, DashboardState, activity_cell_from_tool_ui_event,
        apply_activity_event, assistant_activity_cell, render_activity_from_messages,
        thinking_activity_cell, user_activity_cell_from_event, web_activity_item_from_cell,
    },
    events::{EventPayload, EventStatus, EventView},
    logging::{
        RuntimeStatusLevel, clear_runtime_status, set_runtime_status,
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
            AgentContent, AgentContentPart, AgentMessage, AgentTurnItem, AgentTurnRequest,
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
        ToolExecutionResult, build_runtime_tool_specs, execute_agent_tool_call,
        render_telegram_tool_result_status, render_tool_call_ui_event,
        summarize_action_from_tool_call,
    },
    sleep_status::{
        SleepStatusSnapshot, persist_sleep_status_snapshot, refresh_sleep_status_queues,
    },
    telegram_transport::TelegramLiveDraftClient,
    tool_ui::{ToolCallUiEvent, ToolUiEvent, compact_body_lines},
    workflow::{WorkflowRunRecord, append_workflow_run_records},
    workspace_app::{WorkspaceAppInvalidation, WorkspaceAppRegistry},
};
use chrono::Utc;
use miette::{Result, miette};
use serde_json::json;
use tokio::{sync::mpsc, task::JoinHandle, time::MissedTickBehavior};

use crate::runtime::bootstrap::{
    build_eval_context_with_compiled, load_compiled_prompts_only, summarize_sleep_summary,
};
mod claimed_input;
mod dashboard_control;
mod live_draft;
mod model_driver;
mod scheduler;
mod sleep_driver;
mod turn;
mod workflow_evidence;
mod workspace_apps;

pub(crate) use dashboard_control::handle_dashboard_control_command;
pub(crate) use scheduler::{daat_locus_loop, reset_cancelled_runtime_turn};
pub(crate) use sleep_driver::{SleepTaskResult, handle_sleep_task_result};
pub(crate) use turn::execute_agent_loop_step;
pub(crate) use workflow_evidence::{AgentLoopStepExecution, AgentLoopStepOutput};

use claimed_input::*;
use live_draft::{TelegramLiveDraftSession, maybe_start_telegram_live_draft_session};
use workflow_evidence::{record_runtime_history_messages, record_workflow_run_evidence};
use workspace_apps::{drain_workspace_app_invalidations, sync_workspace_apps_from_invalidation};

const RUNTIME_EVENT_CLAIM_BATCH_SIZE: usize = 1;
const RUNTIME_OVERFLOW_FUSE_THRESHOLD: usize = 3;
const APP_NOTICE_UNRESOLVED_SUPPRESSION_THRESHOLD: usize = 3;
const APP_NOTICE_OVERFLOW_SUPPRESSION: Duration = Duration::from_secs(300);
const RUNTIME_HISTORY_MIN_MESSAGES: usize = 0;
const RUNTIME_HISTORY_SUMMARY_MAX_TOKENS: usize = 800;
const RUNTIME_PREFLIGHT_STAGE_TIMEOUT_SECS: u64 = 60;

#[cfg(test)]
mod tests {
    use super::*;

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
            ClaimedRuntimeInput::Event(EventView {
                event_id: event_a,
                source: crate::events::EventSource::Telegram,
                status: EventStatus::Pending,
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
            }),
            ClaimedRuntimeInput::Event(EventView {
                event_id: event_b,
                source: crate::events::EventSource::Telegram,
                status: EventStatus::Pending,
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
            }),
        ];

        assert_eq!(
            claimed_runtime_input_fingerprint(&inputs).as_deref(),
            Some(
                "events=[00000000-0000-0000-0000-000000000001,00000000-0000-0000-0000-000000000002]|app_notices=[Terminal:busy]"
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
