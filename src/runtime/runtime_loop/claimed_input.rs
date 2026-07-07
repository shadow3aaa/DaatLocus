use super::*;

pub(super) fn runtime_work_origin(inputs: &[ClaimedRuntimeInput]) -> Option<String> {
    if inputs.is_empty() {
        return None;
    }
    if inputs.len() > 1 {
        return Some("runtime_work:batch".to_string());
    }
    inputs.first().map(|event| format!("event:{}", event.event_id))
}

pub(super) type ClaimedRuntimeInput = Box<EventView>;

pub(super) fn claimed_runtime_input_fingerprint(inputs: &[ClaimedRuntimeInput]) -> Option<String> {
    if inputs.is_empty() {
        return None;
    }

    let mut event_ids = inputs
        .iter()
        .map(|input| input.event_id.to_string())
        .collect::<Vec<_>>();
    event_ids.sort();

    Some(format!("events=[{}]", event_ids.join(",")))
}

pub(super) fn claim_pending_runtime_inputs(
    context: &Context,
    max_events: usize,
) -> Vec<ClaimedRuntimeInput> {
    let queued_work = match context.pending_work.claim_batch(max_events) {
        Ok(items) => items,
        Err(err) => {
            tracing::error!("failed to claim pending runtime work batch: {err:?}");
            return Vec::new();
        }
    };

    let mut claimed_inputs = Vec::new();
    for work in queued_work {
        match work {
            PendingWork::Event { event_id } => {
                match context.events.claim_event_if_pending(event_id) {
                    Ok(Some(event)) => {
                        claimed_inputs.push(Box::new(event));
                    }
                    Ok(None) => {
                        if let Err(err) = context
                            .pending_work
                            .consume(PendingWork::Event { event_id })
                        {
                            tracing::error!(
                                "failed to consume stale runtime event driver {event_id}: {err:?}"
                            );
                        }
                    }
                    Err(err) => {
                        tracing::error!(
                            "failed to claim pending runtime event {event_id}: {err:?}"
                        );
                    }
                }
            }
        }
    }
    claimed_inputs
}

pub(super) fn requeue_claimed_runtime_events(context: &Context, event_ids: &[String]) {
    for event_id in event_ids {
        match context.events.requeue_if_claimed(event_id) {
            Ok(true) => {
                if let Ok(event_id) = uuid::Uuid::parse_str(event_id)
                    && let Err(err) = context
                        .pending_work
                        .requeue_front(PendingWork::Event { event_id })
                {
                    tracing::error!(
                        "failed to requeue pending runtime work for event {event_id}: {err:?}"
                    );
                }
            }
            Ok(false) => {}
            Err(err) => {
                tracing::error!("failed to requeue claimed runtime event {event_id}: {err:?}");
            }
        }
    }
}

pub(super) fn handle_runtime_overflow(
    context: &mut Context,
    fingerprint: Option<&str>,
    event_ids: &[String],
    error_text: &str,
) -> bool {
    let Some(fingerprint) = fingerprint else {
        if !event_ids.is_empty() {
            requeue_claimed_runtime_events(context, event_ids);
        }
        return false;
    };

    let attempts = context.record_runtime_overflow_failure(fingerprint);
    if attempts < RUNTIME_OVERFLOW_FUSE_THRESHOLD {
        tracing::warn!(
            overflow_attempt = attempts,
            overflow_threshold = RUNTIME_OVERFLOW_FUSE_THRESHOLD,
            claimed_events = event_ids.join(","),
            "runtime context overflow persisted; requeueing claimed inputs",
        );
        if !event_ids.is_empty() {
            requeue_claimed_runtime_events(context, event_ids);
        }
        return false;
    }

    let failure_note = runtime_overflow_failure_note(attempts, error_text);
    for event_id in event_ids {
        if let Err(err) =
            context
                .events
                .set_status(event_id, EventStatus::Failed, Some(failure_note.clone()))
        {
            tracing::error!("failed to mark overflowed event {event_id} as failed: {err:?}");
        }
        if let Ok(parsed_event_id) = uuid::Uuid::parse_str(event_id)
            && let Err(err) = context.pending_work.consume(PendingWork::Event {
                event_id: parsed_event_id,
            })
        {
            tracing::error!(
                "failed to consume overflowed event driver {event_id} after fuse trip: {err:?}"
            );
        }
    }

    context.clear_runtime_overflow_failure(fingerprint);
    tracing::error!(
        overflow_attempts = attempts,
        overflow_threshold = RUNTIME_OVERFLOW_FUSE_THRESHOLD,
        claimed_events = event_ids.join(","),
        "runtime context overflow fuse tripped; claimed inputs were terminated instead of requeued",
    );
    true
}

pub(super) fn runtime_overflow_failure_note(attempts: usize, error_text: &str) -> String {
    format!("runtime context overflow persisted after {attempts} attempts: {error_text}")
}

pub(super) fn handle_model_request_failure(
    context: &mut Context,
    fingerprint: Option<&str>,
    event_ids: &[String],
    error_text: &str,
    retryable: bool,
) -> bool {
    let Some(fingerprint) = fingerprint else {
        if retryable && !event_ids.is_empty() {
            requeue_claimed_runtime_events(context, event_ids);
            return false;
        }
        terminate_model_request_failure(
            context,
            None,
            1,
            event_ids,
            error_text,
            "non_retryable",
        );
        return true;
    };

    let attempts = context.record_model_request_failure(fingerprint);
    if retryable && attempts < super::RUNTIME_MODEL_REQUEST_FUSE_THRESHOLD {
        tracing::warn!(
            model_request_failure_attempt = attempts,
            model_request_fuse_threshold = super::RUNTIME_MODEL_REQUEST_FUSE_THRESHOLD,
            claimed_events = event_ids.join(","),
            "model request failure persisted; requeueing claimed inputs"
        );
        if !event_ids.is_empty() {
            requeue_claimed_runtime_events(context, event_ids);
        }
        return false;
    }

    terminate_model_request_failure(
        context,
        Some(fingerprint),
        attempts,
        event_ids,
        error_text,
        if retryable {
            "fuse_tripped"
        } else {
            "non_retryable"
        },
    );
    true
}

fn terminate_model_request_failure(
    context: &mut Context,
    fingerprint: Option<&str>,
    attempts: usize,
    event_ids: &[String],
    error_text: &str,
    terminal_reason: &str,
) {
    let failure_note = if terminal_reason == "non_retryable" {
        format!("model request failed with non-retryable error: {error_text}")
    } else {
        format!("model request failed after {attempts} attempts: {error_text}")
    };
    for event_id in event_ids {
        if let Err(err) =
            context
                .events
                .set_status(event_id, EventStatus::Failed, Some(failure_note.clone()))
        {
            tracing::error!(
                "failed to mark event {event_id} as failed after model request fuse: {err:?}"
            );
        }
        if let Ok(parsed_event_id) = uuid::Uuid::parse_str(event_id)
            && let Err(err) = context.pending_work.consume(PendingWork::Event {
                event_id: parsed_event_id,
            })
        {
            tracing::error!(
                "failed to consume event driver {event_id} after model request fuse: {err:?}"
            );
        }
    }

    if let Some(fingerprint) = fingerprint {
        context.clear_model_request_failure(fingerprint);
    }
    tracing::error!(
        model_request_failure_attempts = attempts,
        model_request_fuse_threshold = super::RUNTIME_MODEL_REQUEST_FUSE_THRESHOLD,
        terminal_reason,
        claimed_events = event_ids.join(","),
        "model request failure terminated claimed inputs instead of requeueing"
    );
}

pub(super) fn finalize_claimed_runtime_events(
    context: &Context,
    event_ids: &[String],
    output: &AgentLoopStepOutput,
) {
    if event_ids.is_empty() {
        return;
    }

    let mut requeued = Vec::new();
    for event_id in event_ids {
        match context.events.requeue_if_claimed(event_id) {
            Ok(true) => {
                if let Ok(parsed_event_id) = uuid::Uuid::parse_str(event_id)
                    && let Err(err) = context.pending_work.requeue_front(PendingWork::Event {
                        event_id: parsed_event_id,
                    })
                {
                    tracing::error!(
                        "failed to requeue pending runtime work for event {event_id}: {err:?}"
                    );
                }
                requeued.push(event_id.clone());
            }
            Ok(false) => {}
            Err(err) => {
                tracing::error!("failed to finalize claimed runtime event {event_id}: {err:?}");
            }
        }
    }

    if !requeued.is_empty() {
        let last_action = output.actions.last();
        tracing::debug!(
            action_kind = last_action
                .map(|action| action.kind.as_str())
                .unwrap_or("none"),
            action_summary = last_action
                .map(|action| action.summary.as_str())
                .unwrap_or(""),
            requeued_claimed_events = requeued.len(),
            event_ids = requeued.join(","),
            "requeued claimed runtime events left unresolved at turn end",
        );
    }

    clear_finished_telegram_live_drafts(context, event_ids);
}

fn clear_finished_telegram_live_drafts(context: &Context, event_ids: &[String]) {
    for event_id in event_ids {
        let Ok(event) = context.events.view(event_id) else {
            continue;
        };
        if !matches!(event.payload, EventPayload::TelegramIncoming(_)) {
            continue;
        }
        if !matches!(event.status, EventStatus::Pending | EventStatus::Claimed) {
            context.clear_telegram_live_draft(event_id);
        }
    }
}

pub(super) fn claimed_events_are_terminal(context: &Context, event_ids: &[String]) -> bool {
    if event_ids.is_empty() {
        return false;
    }

    let statuses = event_ids
        .iter()
        .map(|event_id| context.events.view(event_id).map(|event| event.status))
        .collect::<Result<Vec<_>, _>>()
        .ok();
    statuses
        .as_deref()
        .map(claimed_event_statuses_are_terminal)
        .unwrap_or(false)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ClaimedEventStatusSummary {
    pub(super) has_claimed: bool,
    pub(super) all_terminal: bool,
}

pub(super) fn summarize_claimed_event_statuses(
    statuses: &[EventStatus],
) -> ClaimedEventStatusSummary {
    if statuses.is_empty() {
        return ClaimedEventStatusSummary {
            has_claimed: false,
            all_terminal: false,
        };
    }

    let mut all_terminal = true;
    let mut has_claimed = false;

    for status in statuses {
        match status {
            EventStatus::Claimed => {
                has_claimed = true;
                return ClaimedEventStatusSummary {
                    has_claimed,
                    all_terminal: false,
                };
            }
            EventStatus::AwaitingDelivery
            | EventStatus::Resolved
            | EventStatus::Dismissed
            | EventStatus::Failed => {}
            _ => {
                all_terminal = false;
                return ClaimedEventStatusSummary {
                    has_claimed,
                    all_terminal,
                };
            }
        }
    }

    ClaimedEventStatusSummary {
        has_claimed,
        all_terminal,
    }
}

pub(super) fn claimed_event_statuses_are_terminal(statuses: &[EventStatus]) -> bool {
    summarize_claimed_event_statuses(statuses).all_terminal
}

pub(super) fn afterclaim_context_input_for_claimed_inputs(
    inputs: &[ClaimedRuntimeInput],
) -> AfterClaimContextInput {
    let mut context = AfterClaimContextInput::default();
    for input in inputs {
        let event = input;
        context.events.push((**event).clone());
    }
    context
}

pub(super) enum RuntimeFollowUpDecision {
    Continue { reason: RuntimeFollowUpReason },
    AllowFinish,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RuntimeFollowUpReason {
    RawStreamRequestedFollowUp,
    ClaimedEventNeedsExplicitResolution,
}

pub(super) struct RuntimeTurnFollowUpState<'a> {
    pub(super) raw_stream_requested_follow_up: bool,
    pub(super) claimed_statuses: &'a [EventStatus],
}

impl RuntimeFollowUpReason {
    pub(super) fn message(self) -> &'static str {
        match self {
            Self::RawStreamRequestedFollowUp => {
                "This sample is still marked needs_follow_up; continue the current turn."
            }
            Self::ClaimedEventNeedsExplicitResolution => {
                "The current turn has claimed events. Do not end by only outputting text; keep calling tools, and explicitly call `finish_and_send` with `reply_message` when the final reply is ready."
            }
        }
    }
}

pub(super) fn runtime_turn_follow_up_decision(
    context: &Context,
    raw_stream_follow_up: bool,
    claimed_event_ids: &[String],
) -> RuntimeFollowUpDecision {
    let claimed_statuses = claimed_event_ids
        .iter()
        .filter_map(|event_id| context.events.view(event_id).ok().map(|event| event.status))
        .collect::<Vec<_>>();

    let state = RuntimeTurnFollowUpState {
        raw_stream_requested_follow_up: raw_stream_follow_up,
        claimed_statuses: &claimed_statuses,
    };

    runtime_turn_follow_up_decision_from_state(&state)
}

pub(super) fn runtime_turn_follow_up_decision_from_state(
    state: &RuntimeTurnFollowUpState<'_>,
) -> RuntimeFollowUpDecision {
    if state.raw_stream_requested_follow_up {
        return RuntimeFollowUpDecision::Continue {
            reason: RuntimeFollowUpReason::RawStreamRequestedFollowUp,
        };
    }

    if summarize_claimed_event_statuses(state.claimed_statuses).has_claimed {
        return RuntimeFollowUpDecision::Continue {
            reason: RuntimeFollowUpReason::ClaimedEventNeedsExplicitResolution,
        };
    }

    RuntimeFollowUpDecision::AllowFinish
}
