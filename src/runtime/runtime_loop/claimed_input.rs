use super::*;

pub(super) fn runtime_work_origin(inputs: &[ClaimedRuntimeInput]) -> Option<String> {
    if inputs.is_empty() {
        return None;
    }
    if inputs.len() > 1 {
        return Some("runtime_work:batch".to_string());
    }
    match inputs.first() {
        Some(ClaimedRuntimeInput::Event(event)) => Some(format!("event:{}", event.event_id)),
        Some(ClaimedRuntimeInput::AppNotice { app, reason }) => {
            Some(format!("app_notice:{app}:{}", reason.trim()))
        }
        None => None,
    }
}

pub(super) enum ClaimedRuntimeInput {
    Event(EventView),
    AppNotice { app: AppId, reason: String },
}

pub(super) fn claimed_runtime_input_fingerprint(inputs: &[ClaimedRuntimeInput]) -> Option<String> {
    if inputs.is_empty() {
        return None;
    }

    let mut event_ids = inputs
        .iter()
        .filter_map(|input| match input {
            ClaimedRuntimeInput::Event(event) => Some(event.event_id.to_string()),
            ClaimedRuntimeInput::AppNotice { .. } => None,
        })
        .collect::<Vec<_>>();
    event_ids.sort();

    let mut app_notices = inputs
        .iter()
        .filter_map(|input| match input {
            ClaimedRuntimeInput::Event(_) => None,
            ClaimedRuntimeInput::AppNotice { app, reason } => {
                Some(format!("{app}:{}", reason.trim()))
            }
        })
        .collect::<Vec<_>>();
    app_notices.sort();

    Some(format!(
        "events=[{}]|app_notices=[{}]",
        event_ids.join(","),
        app_notices.join(","),
    ))
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
                    Ok(Some(event)) => claimed_inputs.push(ClaimedRuntimeInput::Event(event)),
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
            PendingWork::AppNotice { app, reason } => {
                let Some(current_reason) = context.apps.notice_reason(&app) else {
                    if let Err(err) = context.pending_work.consume(PendingWork::AppNotice {
                        app: app.clone(),
                        reason: String::new(),
                    }) {
                        tracing::error!(
                            "failed to consume stale app notice driver for {app}: {err:?}"
                        );
                    }
                    continue;
                };
                let reason = if current_reason.trim().is_empty() {
                    reason
                } else {
                    current_reason
                };
                claimed_inputs.push(ClaimedRuntimeInput::AppNotice { app, reason });
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
    app_notices: &[(AppId, String)],
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
            claimed_app_notices = app_notices
                .iter()
                .map(|(app, _)| app.to_string())
                .collect::<Vec<_>>()
                .join(","),
            "runtime context overflow persisted; requeueing claimed inputs",
        );
        if !event_ids.is_empty() {
            requeue_claimed_runtime_events(context, event_ids);
        }
        return false;
    }

    let failure_note =
        format!("runtime context overflow persisted after {attempts} attempts: {error_text}");
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

    for (app, reason) in app_notices {
        context.suppress_app_notice(app, reason.clone(), APP_NOTICE_OVERFLOW_SUPPRESSION);
        context.active_app_notices.remove(app);
        if let Err(err) = context.pending_work.consume(PendingWork::AppNotice {
            app: app.clone(),
            reason: String::new(),
        }) {
            tracing::error!(
                "failed to consume overflowed app notice driver for {app} after fuse trip: {err:?}"
            );
        }
    }

    context.clear_runtime_overflow_failure(fingerprint);
    tracing::error!(
        overflow_attempts = attempts,
        overflow_threshold = RUNTIME_OVERFLOW_FUSE_THRESHOLD,
        suppression_secs = APP_NOTICE_OVERFLOW_SUPPRESSION.as_secs(),
        claimed_events = event_ids.join(","),
        claimed_app_notices = app_notices
            .iter()
            .map(|(app, _)| app.to_string())
            .collect::<Vec<_>>()
            .join(","),
        "runtime context overflow fuse tripped; claimed inputs were terminated instead of requeued",
    );
    true
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
        tracing::info!(
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
}

pub(super) async fn finalize_claimed_runtime_app_notices(
    context: &mut Context,
    apps: &[AppId],
    output: &AgentLoopStepOutput,
) {
    if apps.is_empty() {
        return;
    }

    let mut released = Vec::new();
    for app in apps {
        if let Err(err) = context.apps.refresh_notice_for(app).await {
            tracing::error!("failed to refresh app notice for {app}: {err:?}");
        }
        let still_noticed = context.apps.notice_reason(app).is_some();
        let work = PendingWork::AppNotice {
            app: app.clone(),
            reason: String::new(),
        };
        if still_noticed {
            match context.pending_work.release_claimed(work) {
                Ok(true) => released.push(app.to_string()),
                Ok(false) => {}
                Err(err) => {
                    tracing::error!(
                        "failed to release claimed app notice driver for {app}: {err:?}"
                    );
                }
            }
        } else if let Err(err) = context.pending_work.consume(work) {
            tracing::error!("failed to consume app notice driver for {app}: {err:?}");
        }
    }

    if !released.is_empty() {
        let last_action = output.actions.last();
        tracing::info!(
            action_kind = last_action
                .map(|action| action.kind.as_str())
                .unwrap_or("none"),
            action_summary = last_action
                .map(|action| action.summary.as_str())
                .unwrap_or(""),
            reactivated_app_notice_drivers = released.len(),
            apps = released.join(","),
            "released claimed runtime app notice drivers back into frontier at turn end",
        );
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

pub(super) fn prompt_message_for_claimed_input(
    _context: &Context,
    input: &ClaimedRuntimeInput,
) -> HistoryMessage {
    match input {
        ClaimedRuntimeInput::Event(event) => match &event.payload {
            EventPayload::TelegramIncoming(payload) => HistoryMessage::user(format!(
                "<world_event source=\"telegram\" event_id=\"{}\" status=\"{}\">\nfrom: {}\nchat_title: {}\nchat_id: {}\nincoming_text: {}\n</world_event>",
                event.event_id,
                event.status,
                payload.sender,
                payload.chat_title,
                payload.chat_id,
                payload.incoming_text.trim(),
            )),
            EventPayload::TerminalIncoming(payload) => HistoryMessage::user(format!(
                "<world_event source=\"terminal\" event_id=\"{}\" status=\"{}\">\norigin: {}\nincoming_text: {}\n</world_event>",
                event.event_id,
                event.status,
                payload.origin,
                payload.incoming_text.trim(),
            )),
        },
        ClaimedRuntimeInput::AppNotice { app, reason } => HistoryMessage::user(format!(
            "<app_notice app=\"{}\">\nreason: {}\n</app_notice>",
            app, reason,
        )),
    }
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
                "本次采样仍标记为 needs_follow_up；请继续推进当前 turn。"
            }
            Self::ClaimedEventNeedsExplicitResolution => {
                "当前 turn 已领取事件。不要只输出文本回复来结束；请继续调用工具，并在准备好最终答复时显式调用 `finish_and_send` 提交 reply_message。"
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
