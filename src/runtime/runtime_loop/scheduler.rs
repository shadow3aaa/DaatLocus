use super::sleep_driver::{maybe_start_forced_sleep, maybe_start_idle_sleep};
use super::*;

pub(crate) enum RuntimeLoopCycle {
    Idle,
    ProcessedWork,
}

pub(crate) async fn daat_locus_loop(
    context: &mut Context,
    tx: &tokio::sync::watch::Sender<DashboardState>,
    sleep_result_tx: &tokio::sync::mpsc::UnboundedSender<SleepTaskResult>,
    session_title_result_tx: &tokio::sync::mpsc::UnboundedSender<
        crate::runtime::session_title::SessionTitleGenerationResult,
    >,
    sleep_running: &mut bool,
    sleep_status: &mut SleepStatusSnapshot,
) -> RuntimeLoopCycle {
    let cycle_started_at = std::time::Instant::now();
    sync_workspace_apps_from_invalidation(context).await;

    let forced_sleep_status =
        maybe_start_forced_sleep(context, tx, sleep_result_tx, sleep_running, sleep_status).await;
    refresh_sleep_status_queues(sleep_status).await;
    sync_driver_frontier_from_sources(context);
    if context.active_runtime_turn {
        tracing::warn!(
            elapsed_secs = context
                .runtime_turn_started_at
                .map(|t| t.elapsed().as_secs())
                .unwrap_or(0),
            phase = context
                .active_runtime_phase
                .map(|phase| phase.label())
                .unwrap_or("running"),
            "stale active_runtime_turn detected at loop entry; resetting cancelled turn"
        );
        reset_cancelled_runtime_turn(context, "stale active_runtime_turn at loop entry");
    }
    let pending_work_count = context.pending_work.pending_count();
    if pending_work_count == 0 {
        if context.idle_since.is_none() {
            context.idle_since = Some(std::time::Instant::now());
        }
        crate::runtime::session_title::spawn_session_title_generation(
            context,
            session_title_result_tx,
        );
        if let Some(status) =
            maybe_start_idle_sleep(context, tx, sleep_result_tx, sleep_running, sleep_status).await
        {
            set_runtime_status_only(Some(tx), status);
        } else if let Some(status) = forced_sleep_status {
            set_runtime_status_only(Some(tx), status);
        } else {
            clear_runtime_status(Some(tx));
        }
        sync_dashboard_state(
            context,
            tx,
            sleep_status,
            Some(cycle_started_at.elapsed().as_millis()),
        );
        return RuntimeLoopCycle::Idle;
    }
    context.idle_since = None;
    let mut status = format!("processing: {pending_work_count} pending work item(s)");
    if let Some(forced_sleep_status) = forced_sleep_status.as_deref() {
        status.push_str(" | ");
        status.push_str(forced_sleep_status);
    }
    set_runtime_status_only(Some(tx), status);
    context
        .apps
        .wait_until_settled(Duration::from_secs(1), Duration::from_secs(3))
        .await;
    let runtime_turn_started_at = std::time::Instant::now();
    context.active_runtime_turn = true;
    context.runtime_turn_epoch = context.runtime_turn_epoch.wrapping_add(1);
    context.runtime_turn_started_at = Some(runtime_turn_started_at);
    context.runtime_turn_started_at_ms = Some(chrono::Utc::now().timestamp_millis());
    context.set_runtime_phase(Some(RuntimeTurnPhase::PreflightPreTurnContext));
    sync_dashboard_state(
        context,
        tx,
        sleep_status,
        Some(cycle_started_at.elapsed().as_millis()),
    );
    let _ = execute_agent_loop_step(context, Some(tx)).await;
    context.active_runtime_turn = false;
    context.runtime_turn_started_at = None;
    context.runtime_turn_started_at_ms = None;
    crate::runtime::session_title::spawn_session_title_generation(context, session_title_result_tx);
    sync_dashboard_state(
        context,
        tx,
        sleep_status,
        Some(cycle_started_at.elapsed().as_millis()),
    );
    RuntimeLoopCycle::ProcessedWork
}

fn sync_driver_frontier_from_sources(context: &Context) {
    for (event_id, status) in context.events.driver_event_statuses() {
        let work = PendingWork::Event { event_id };
        if matches!(status, crate::events::EventStatus::Pending) {
            if let Err(err) = context.pending_work.enqueue(work) {
                tracing::error!("failed to sync pending event driver {event_id}: {err:?}");
            }
        } else if let Err(err) = context.pending_work.consume(work) {
            tracing::error!("failed to remove stale event driver {event_id}: {err:?}");
        }
    }
}

fn recover_stale_runtime_turn_claims(context: &mut Context) {
    let mut claimed_event_ids = std::mem::take(&mut context.claimed_event_ids);
    if claimed_event_ids.is_empty() {
        claimed_event_ids = context
            .events
            .driver_event_statuses()
            .into_iter()
            .filter(|(_, status)| matches!(status, EventStatus::Claimed))
            .map(|(event_id, _)| event_id.to_string())
            .collect();
    }
    if !claimed_event_ids.is_empty() {
        requeue_claimed_runtime_events(context, &claimed_event_ids);
    }

    context.install_live_progress(None);
    context.current_work_origin = None;
}

pub(crate) fn reset_cancelled_runtime_turn(context: &mut Context, reason: &str) {
    recover_stale_runtime_turn_claims(context);
    tracing::warn!(reason, "reset cancelled active runtime turn");
    context.active_runtime_turn = false;
    context.set_runtime_phase(None);
    context.runtime_turn_started_at = None;
    context.runtime_turn_started_at_ms = None;
}

pub(crate) fn interrupt_active_runtime_turn(context: &mut Context, reason: &str) -> usize {
    let mut claimed_event_ids = std::mem::take(&mut context.claimed_event_ids);
    if claimed_event_ids.is_empty() {
        claimed_event_ids = context
            .events
            .driver_event_statuses()
            .into_iter()
            .filter(|(_, status)| matches!(status, EventStatus::Claimed))
            .map(|(event_id, _)| event_id.to_string())
            .collect();
    }
    let mut failed_events = 0usize;
    for event_id in claimed_event_ids {
        if let Err(err) = context.events.set_status(
            &event_id,
            EventStatus::Failed,
            Some(format!("runtime turn interrupted by user: {reason}")),
        ) {
            tracing::error!("failed to mark interrupted runtime event {event_id} failed: {err:?}");
        } else {
            failed_events += 1;
        }
        if let Ok(parsed_event_id) = uuid::Uuid::parse_str(&event_id)
            && let Err(err) = context.pending_work.consume(PendingWork::Event {
                event_id: parsed_event_id,
            })
        {
            tracing::error!(
                "failed to consume interrupted runtime event driver {event_id}: {err:?}"
            );
        }
    }

    if failed_events > 0 {
        tracing::warn!(
            reason,
            failed_events,
            "interrupted active runtime turn and terminated claimed inputs"
        );
    } else {
        tracing::warn!(
            reason,
            "interrupted active runtime turn with no claimed inputs"
        );
    }

    context.install_live_progress(None);
    context.current_work_origin = None;
    context.active_runtime_turn = false;
    context.set_runtime_phase(None);
    context.runtime_turn_started_at = None;
    context.runtime_turn_started_at_ms = None;

    failed_events
}
