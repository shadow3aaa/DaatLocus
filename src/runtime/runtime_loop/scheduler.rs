use super::sleep_driver::{maybe_start_forced_sleep, maybe_start_idle_sleep};
use super::*;

pub(crate) async fn daat_locus_loop(
    context: &mut Context,
    tx: &tokio::sync::watch::Sender<DashboardState>,
    sleep_result_tx: &tokio::sync::mpsc::UnboundedSender<SleepTaskResult>,
    sleep_running: &mut bool,
    sleep_status: &mut SleepDashboardStatus,
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
    refresh_sleep_backlogs(sleep_status).await;
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
            context.active_runtime_turn = false;
            context.set_runtime_phase(None);
            context.runtime_turn_started_at = None;
            // fall through to normal processing
        } else {
            let phase = context
                .active_runtime_phase
                .map(|phase| phase.label())
                .unwrap_or("running");
            set_runtime_status(
                Some(tx),
                RuntimeStatusLevel::Info,
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
    let submitted_handoffs = context.hindsight_retain.drain_submitted().await;
    context
        .memory
        .mark_handoffs_submitted(&submitted_handoffs)
        .await;
    if context.memory.should_block_new_turns_on_handoff_backlog() {
        let handoff_backlog = context.memory.handoff_backlog_count();
        set_runtime_status(
            Some(tx),
            RuntimeStatusLevel::Info,
            format!(
                "processing: waiting for hindsight handoff backlog ({handoff_backlog} turn(s))"
            ),
        );
        sync_dashboard_state(
            context,
            tx,
            sleep_status,
            Some(cycle_started_at.elapsed().as_millis()),
        );
        match context.hindsight_retain.flush().await {
            Ok(submitted_handoffs) => {
                context
                    .memory
                    .mark_handoffs_submitted(&submitted_handoffs)
                    .await;
            }
            Err(err) => {
                tracing::error!("failed to flush hindsight handoff queue before new turn: {err:?}");
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
        return;
    }
    let pending_work_count = context.pending_work.pending_count();
    if pending_work_count == 0 {
        if context.idle_since.is_none() {
            context.idle_since = Some(std::time::Instant::now());
        }
        if let Some(status) =
            maybe_start_idle_sleep(context, tx, sleep_result_tx, sleep_running, sleep_status).await
        {
            set_runtime_status(Some(tx), RuntimeStatusLevel::Info, status);
        } else if let Some(status) = forced_sleep_status {
            set_runtime_status(Some(tx), RuntimeStatusLevel::Info, status);
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
    set_runtime_status(Some(tx), RuntimeStatusLevel::Info, status);
    context
        .apps
        .wait_until_settled(Duration::from_secs(1), Duration::from_secs(3))
        .await;
    context.active_runtime_turn = true;
    context.runtime_turn_started_at = Some(std::time::Instant::now());
    context.set_runtime_phase(Some(RuntimeTurnPhase::PreflightMemory));
    sync_dashboard_state(
        context,
        tx,
        sleep_status,
        Some(cycle_started_at.elapsed().as_millis()),
    );
    let _ = execute_agent_loop_step(context, Some(tx)).await;
    context.active_runtime_turn = false;
    context.runtime_turn_started_at = None;
    context.set_runtime_phase(None);
    refresh_sleep_backlogs(sleep_status).await;
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
