use super::sleep_driver::{maybe_start_forced_sleep, maybe_start_idle_sleep};
use super::*;

pub(crate) async fn daat_locus_loop(
    context: &mut Context,
    tx: &tokio::sync::watch::Sender<DashboardState>,
    sleep_result_tx: &tokio::sync::mpsc::UnboundedSender<SleepTaskResult>,
    sleep_running: &mut bool,
    sleep_status: &mut SleepStatusSnapshot,
    workspace_app_invalidation_rx: &mut tokio::sync::mpsc::UnboundedReceiver<
        WorkspaceAppInvalidation,
    >,
) {
    let cycle_started_at = std::time::Instant::now();
    drain_workspace_app_invalidations(&mut context.workspace_apps, workspace_app_invalidation_rx);
    sync_workspace_apps_from_invalidation(context).await;
    if let Err(err) = context.apps.refresh_all_notices().await {
        tracing::error!("failed to refresh app notices: {err:?}");
    }
    refresh_sleep_status_queues(sleep_status).await;
    let forced_sleep_status =
        maybe_start_forced_sleep(context, tx, sleep_result_tx, sleep_running, sleep_status).await;
    enqueue_app_notice_work(context);
    sync_driver_frontier_from_sources(context);
    if context.active_runtime_turn {
        // Detect a stale flag caused by select! cancellation. If the turn has
        // exceeded request_timeout + 120s but active_runtime_turn is still true,
        // daat_locus_loop was likely cancelled before resetting it.
        let stale_threshold = Duration::from_secs(
            context
                .config
                .main_model_config()
                .request_timeout_secs()
                .saturating_add(120),
        );
        let is_stale = context
            .runtime_turn_started_at
            .map(|started| started.elapsed() > stale_threshold)
            .unwrap_or(false);
        if is_stale {
            tracing::warn!(
                elapsed_secs = context
                    .runtime_turn_started_at
                    .map(|t| t.elapsed().as_secs())
                    .unwrap_or(0),
                threshold_secs = stale_threshold.as_secs(),
                "stale active_runtime_turn detected (likely cancelled by tokio::select!); resetting"
            );
            reset_cancelled_runtime_turn(context, "stale active_runtime_turn");
            // fall through to normal processing
        } else {
            let phase = context
                .active_runtime_phase
                .map(|phase| phase.label())
                .unwrap_or("running");
            set_runtime_status_only(
                Some(tx),
                format!("processing: runtime turn running / {phase}"),
            );
            sync_dashboard_state(
                context,
                tx,
                sleep_status,
                Some(cycle_started_at.elapsed().as_millis()),
            );
            tokio::time::sleep(Duration::from_millis(250)).await;
            return;
        }
    }
    let pending_work_count = context.pending_work.pending_count();
    if pending_work_count == 0 {
        if context.idle_since.is_none() {
            context.idle_since = Some(std::time::Instant::now());
        }
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
        tokio::time::sleep(Duration::from_secs(2)).await;
        return;
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
    let (activity_len_before_turn, last_activity_cell_before_turn) = {
        let state = tx.borrow();
        (
            state.activity_cells.len(),
            state.activity_cells.last().cloned(),
        )
    };
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
    if let Err(err) =
        crate::runtime::session_title::refresh_session_title_after_activity(context, tx).await
    {
        tracing::warn!("session title refresh failed: {err:?}");
    }
    super::turn::append_final_message_separator_activity_cell(
        context,
        tx,
        activity_len_before_turn,
        last_activity_cell_before_turn,
        Some(runtime_turn_started_at.elapsed().as_secs()),
    );
    context.active_runtime_turn = false;
    context.runtime_turn_started_at = None;
    context.runtime_turn_started_at_ms = None;
    context.set_runtime_phase(None);
    clear_runtime_status(Some(tx));
    refresh_sleep_status_queues(sleep_status).await;
    sync_dashboard_state(
        context,
        tx,
        sleep_status,
        Some(cycle_started_at.elapsed().as_millis()),
    );
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

    let claimed_app_notices = std::mem::take(&mut context.claimed_app_notices);
    let mut released_app_notices = Vec::new();
    for notice in claimed_app_notices {
        let work = PendingWork::AppNotice {
            app: notice.app.clone(),
            reason: notice.reason.clone(),
        };
        match context.pending_work.release_claimed(work.clone()) {
            Ok(true) => {
                released_app_notices.push(format!("{}:{}", notice.app, notice.reason));
            }
            Ok(false) => {
                let current_reason = context
                    .apps
                    .notice_reason(&notice.app)
                    .and_then(|reason| crate::context::normalize_app_notice_reason(&reason));
                if current_reason.as_deref() == Some(notice.reason.as_str())
                    && !context.app_notice_is_resolved(&notice)
                {
                    if let Err(err) = context.pending_work.requeue_front(work) {
                        let app = &notice.app;
                        tracing::error!(
                            "failed to requeue stale runtime app notice driver for {app}: {err:?}"
                        );
                    } else {
                        released_app_notices.push(format!("{}:{}", notice.app, notice.reason));
                    }
                }
            }
            Err(err) => {
                let app = &notice.app;
                tracing::error!(
                    "failed to release stale runtime app notice driver for {app}: {err:?}"
                );
            }
        }
    }

    if !claimed_event_ids.is_empty() || !released_app_notices.is_empty() {
        tracing::warn!(
            requeued_claimed_events = claimed_event_ids.len(),
            event_ids = claimed_event_ids.join(","),
            requeued_app_notices = released_app_notices.len(),
            app_notices = released_app_notices.join(","),
            "requeued claimed runtime inputs after stale turn reset"
        );
    }
    context.install_live_progress(None);
    context.current_work_origin = None;
    context.workflow_step_started_bound_id = None;
}

pub(crate) fn reset_cancelled_runtime_turn(context: &mut Context, reason: &str) {
    recover_stale_runtime_turn_claims(context);
    tracing::warn!(reason, "reset cancelled active runtime turn");
    context.active_runtime_turn = false;
    context.set_runtime_phase(None);
    context.runtime_turn_started_at = None;
    context.runtime_turn_started_at_ms = None;
}

fn enqueue_app_notice_work(context: &mut Context) {
    for app_id in context.apps.app_ids() {
        let Some(reason) = context
            .apps
            .notice_reason(&app_id)
            .and_then(|reason| crate::context::normalize_app_notice_reason(&reason))
        else {
            context.clear_active_app_notice(&app_id);
            context.clear_app_notice_suppression(&app_id);
            if let Err(err) = context.pending_work.consume(PendingWork::AppNotice {
                app: app_id.clone(),
                reason: String::new(),
            }) {
                tracing::error!("failed to remove cleared app notice work for {app_id}: {err:?}");
            }
            continue;
        };

        if context.is_app_notice_suppressed(&app_id, &reason) {
            context.clear_active_app_notice(&app_id);
            if let Err(err) = context.pending_work.consume(PendingWork::AppNotice {
                app: app_id.clone(),
                reason: String::new(),
            }) {
                tracing::error!(
                    "failed to remove suppressed app notice work for {app_id}: {err:?}"
                );
            }
            continue;
        }

        let key = AppNoticeKey::new(app_id.clone(), reason.clone());
        let should_enqueue = match context.active_app_notices.get(&key) {
            Some(active) if active.resolved => {
                if let Err(err) = context.pending_work.consume(PendingWork::AppNotice {
                    app: app_id.clone(),
                    reason: String::new(),
                }) {
                    tracing::error!(
                        "failed to remove resolved app notice work for {app_id}: {err:?}"
                    );
                }
                false
            }
            Some(_) => false,
            None => true,
        };

        if should_enqueue {
            context.activate_app_notice(app_id.clone(), reason.clone());
            if let Err(err) = context.pending_work.enqueue(PendingWork::AppNotice {
                app: app_id.clone(),
                reason,
            }) {
                tracing::error!("failed to enqueue app notice work for {app_id}: {err:?}");
            }
        }
    }
}
