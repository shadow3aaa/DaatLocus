use super::sleep_driver::{SleepTrigger, start_background_sleep};
use super::*;
use crate::daemon::DaemonControlCommand;

pub(crate) async fn handle_dashboard_control_command(
    context: &mut Context,
    tx: &tokio::sync::watch::Sender<DashboardState>,
    sleep_result_tx: &tokio::sync::mpsc::UnboundedSender<SleepTaskResult>,
    sleep_running: &mut bool,
    sleep_status: &mut SleepStatusSnapshot,
    command: DashboardControlCommand,
) {
    match command {
        DashboardControlCommand::RunSleep => {
            if *sleep_running {
                set_runtime_status_only(Some(tx), "sleep is already running in the background");
                sync_dashboard_state(context, tx, sleep_status, None);
                return;
            }
            start_background_sleep(
                context,
                tx,
                sleep_result_tx,
                sleep_running,
                sleep_status,
                SleepTrigger::Manual,
                "running sleep in the background",
            )
            .await;
        }
        DashboardControlCommand::RestartDaemon => {
            set_runtime_status(
                Some(tx),
                RuntimeStatusLevel::Info,
                "daemon restart requested from command system",
            );
            sync_dashboard_state(context, tx, sleep_status, None);
            match context
                .daemon_control_tx
                .send(DaemonControlCommand::RestartRequested)
            {
                Ok(()) => {}
                Err(err) => {
                    tracing::error!("failed to queue daemon restart from dashboard command: {err}");
                    set_runtime_status(
                        Some(tx),
                        RuntimeStatusLevel::Error,
                        format!("failed to queue daemon restart: {err}"),
                    );
                    sync_dashboard_state(context, tx, sleep_status, None);
                }
            }
        }
        DashboardControlCommand::ClearConversation => {
            let cleared_events = match context.events.clear_all() {
                Ok(count) => count,
                Err(err) => {
                    tracing::error!("failed to clear events during /clear: {err:?}");
                    0
                }
            };
            let cleared_event_work = match context.pending_work.clear_events() {
                Ok(count) => count,
                Err(err) => {
                    tracing::error!("failed to clear event pending work during /clear: {err:?}");
                    0
                }
            };
            let cleared_outbound = match context.telegram.clear_outbox() {
                Ok(count) => count,
                Err(err) => {
                    tracing::error!("failed to clear telegram outbox during /clear: {err:?}");
                    0
                }
            };
            let cleared_live_drafts = {
                let mut live_drafts = context.telegram_live_drafts.lock();
                let count = live_drafts.len();
                live_drafts.clear();
                count
            };
            let cleared_dashboard_history = match context.dashboard_history.as_ref() {
                Some(history) => match history.clear_all() {
                    Ok(count) => count,
                    Err(err) => {
                        tracing::error!(
                            "failed to clear dashboard activity history during /clear: {err:?}"
                        );
                        0
                    }
                },
                None => 0,
            };
            if let Some(session) = context.active_primitive_run.as_mut() {
                session.final_summary = "abandoned by dashboard /clear".to_string();
            }
            context.queue_active_primitive_run_for_flush(
                crate::workflow::PrimitiveRunOutcome::Abandoned,
            );
            context.bound_primitive_id = None;
            context.bound_primitive_composition = None;
            context.install_live_progress(None);
            context.claimed_event_ids.clear();
            context.active_runtime_turn = false;
            context.set_runtime_phase(None);
            context.runtime_turn_started_at = None;
            context.current_work_origin = None;
            context.workflow_step_started_bound_id = None;
            context.memory.clear_runtime_conversation().await;
            if context.plan.clear()
                && let Err(err) = context.plan.sync_to_disk().await
            {
                tracing::error!("failed to persist cleared plan: {err}");
            }
            tx.send_modify(|state| {
                state.activity_history = DashboardActivityHistoryWindow::default();
                state.activity_cells.clear();
                state.live_activity_cells.clear();
                state.web_activity_items.clear();
                state.live_web_activity_items.clear();
                crate::dashboard::sync_web_activity_state(state);
            });
            set_runtime_status(
                Some(tx),
                RuntimeStatusLevel::Info,
                format!(
                    "runtime conversation, current plan, events, and dashboard activity cleared (events={cleared_events}, event_work={cleared_event_work}, telegram_outbox={cleared_outbound}, live_drafts={cleared_live_drafts}, activity_items={cleared_dashboard_history})"
                ),
            );
            sync_dashboard_state(context, tx, sleep_status, None);
        }
    }
}
