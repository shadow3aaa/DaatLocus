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
            PendingWork::AppNotice { app, reason: _ } => {
                let Some(current_reason) = context
                    .apps
                    .notice_reason(&app)
                    .and_then(|reason| crate::context::normalize_app_notice_reason(&reason))
                else {
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
                if context.is_app_notice_suppressed(&app, &current_reason) {
                    if let Err(err) = context.pending_work.consume(PendingWork::AppNotice {
                        app: app.clone(),
                        reason: String::new(),
                    }) {
                        tracing::error!(
                            "failed to consume suppressed app notice driver for {app}: {err:?}"
                        );
                    }
                    continue;
                }
                let reason = current_reason;
                let key = AppNoticeKey::new(app.clone(), reason.clone());
                if context.app_notice_is_resolved(&key) {
                    if let Err(err) = context.pending_work.consume(PendingWork::AppNotice {
                        app: app.clone(),
                        reason: String::new(),
                    }) {
                        tracing::error!(
                            "failed to consume already resolved app notice driver for {app}: {err:?}"
                        );
                    }
                    continue;
                }
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
    app_notices: &[AppNoticeKey],
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
                .map(|notice| notice.app.to_string())
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

    for notice in app_notices {
        context.suppress_app_notice(
            &notice.app,
            notice.reason.clone(),
            APP_NOTICE_OVERFLOW_SUPPRESSION,
        );
        context.clear_active_app_notice(&notice.app);
        if let Err(err) = context.pending_work.consume(PendingWork::AppNotice {
            app: notice.app.clone(),
            reason: String::new(),
        }) {
            let app = &notice.app;
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
            .map(|notice| notice.app.to_string())
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
    notices: &[AppNoticeKey],
    output: &AgentLoopStepOutput,
) {
    if notices.is_empty() {
        return;
    }

    let mut released = Vec::new();
    let mut resolved = Vec::new();
    let mut suppressed = Vec::new();
    for notice in notices {
        let app = &notice.app;
        if let Err(err) = context.apps.refresh_notice_for(app).await {
            tracing::error!("failed to refresh app notice for {app}: {err:?}");
        }
        let work = PendingWork::AppNotice {
            app: app.clone(),
            reason: notice.reason.clone(),
        };

        if context.app_notice_is_resolved(notice) {
            if let Err(err) = context.pending_work.consume(work) {
                tracing::error!("failed to consume resolved app notice driver for {app}: {err:?}");
            } else {
                resolved.push(format!("{app}:{}", notice.reason));
            }
            continue;
        }

        let current_reason = context
            .apps
            .notice_reason(app)
            .and_then(|reason| crate::context::normalize_app_notice_reason(&reason));

        match current_reason {
            Some(current_reason) if current_reason == notice.reason => {
                let attempts = context.record_unresolved_app_notice_turn(notice);
                if attempts >= APP_NOTICE_UNRESOLVED_SUPPRESSION_THRESHOLD {
                    context.suppress_app_notice(
                        app,
                        notice.reason.clone(),
                        APP_NOTICE_OVERFLOW_SUPPRESSION,
                    );
                    context.clear_active_app_notice(app);
                    if let Err(err) = context.pending_work.consume(work) {
                        tracing::error!(
                            "failed to consume suppressed unresolved app notice driver for {app}: {err:?}"
                        );
                    }
                    suppressed.push(format!("{app}:{}", notice.reason));
                    continue;
                }

                match context.pending_work.release_claimed(work) {
                    Ok(true) => released.push(app.to_string()),
                    Ok(false) => {}
                    Err(err) => {
                        tracing::error!(
                            "failed to release claimed app notice driver for {app}: {err:?}"
                        );
                    }
                }
            }
            Some(current_reason) => {
                context.activate_app_notice(app.clone(), current_reason.clone());
                if let Err(err) = context.pending_work.requeue_front(PendingWork::AppNotice {
                    app: app.clone(),
                    reason: current_reason.clone(),
                }) {
                    tracing::error!(
                        "failed to requeue changed app notice driver for {app}: {err:?}"
                    );
                }
                released.push(format!("{app}:{current_reason}"));
            }
            None => {
                context.clear_active_app_notice(app);
                if let Err(err) = context.pending_work.consume(work) {
                    tracing::error!(
                        "failed to consume cleared app notice driver for {app}: {err:?}"
                    );
                }
            }
        }
    }

    if !resolved.is_empty() {
        tracing::info!(
            resolved_app_notice_drivers = resolved.len(),
            app_notices = resolved.join(","),
            "consumed explicitly resolved runtime app notice drivers",
        );
    }

    if !suppressed.is_empty() {
        tracing::warn!(
            suppression_secs = APP_NOTICE_OVERFLOW_SUPPRESSION.as_secs(),
            suppressed_app_notice_drivers = suppressed.len(),
            app_notices = suppressed.join(","),
            "suppressed repeatedly unresolved runtime app notice drivers",
        );
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
            "<app_notice app=\"{}\">\nreason: {}\nresolution: call `notice_resolved` with this app and reason when handled\n</app_notice>",
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
    ClaimedAppNoticeNeedsExplicitResolution,
}

pub(super) struct RuntimeTurnFollowUpState<'a> {
    pub(super) raw_stream_requested_follow_up: bool,
    pub(super) claimed_statuses: &'a [EventStatus],
    pub(super) has_claimed_app_notice: bool,
    pub(super) claimed_app_notice_resolved: bool,
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
            Self::ClaimedAppNoticeNeedsExplicitResolution => {
                "The current turn has claimed an app notice. Do not end by only outputting text; keep calling tools, and explicitly call `notice_resolved` for the claimed app and reason when the notice has been handled."
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
        has_claimed_app_notice: !context.claimed_app_notices.is_empty(),
        claimed_app_notice_resolved: context.claimed_app_notices_are_resolved(),
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

    if state.has_claimed_app_notice && !state.claimed_app_notice_resolved {
        return RuntimeFollowUpDecision::Continue {
            reason: RuntimeFollowUpReason::ClaimedAppNoticeNeedsExplicitResolution,
        };
    }

    RuntimeFollowUpDecision::AllowFinish
}
