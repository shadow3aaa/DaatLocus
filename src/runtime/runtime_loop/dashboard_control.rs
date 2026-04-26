use super::sleep_driver::{SleepTrigger, start_background_sleep};
use super::*;

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
                set_runtime_status(
                    Some(tx),
                    RuntimeStatusLevel::Info,
                    "sleep is already running in the background",
                );
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
            if let Some(session) = context.active_workflow_run.as_mut() {
                session.final_summary = "abandoned by dashboard /clear".to_string();
            }
            context.queue_active_workflow_run_for_flush(
                crate::workflow::WorkflowRunOutcome::Abandoned,
            );
            context.bound_workflow_id = None;
            context.install_live_progress(None);
            context.claimed_event_ids.clear();
            context.active_runtime_turn = false;
            context.set_runtime_phase(None);
            context.runtime_turn_started_at = None;
            context.current_work_origin = None;
            context.workflow_step_started_bound_id = None;
            let retain_plan = context.memory.clear_runtime_conversation().await;
            if context.plan.clear()
                && let Err(err) = context.plan.sync_to_disk().await
            {
                tracing::error!("failed to persist cleared plan: {err}");
            }
            for job in retain_plan.jobs {
                if let Err(err) = context.hindsight_retain.enqueue(job) {
                    tracing::error!("failed to enqueue hindsight retain job during clear: {err:?}");
                }
            }
            if retain_plan.must_flush_before_continue || context.memory.handoff_backlog_count() > 0
            {
                match context.hindsight_retain.flush().await {
                    Ok(submitted_handoffs) => {
                        context
                            .memory
                            .mark_handoffs_submitted(&submitted_handoffs)
                            .await;
                    }
                    Err(err) => {
                        tracing::error!(
                            "failed to flush hindsight handoff queue during clear: {err:?}"
                        );
                    }
                }
            }
            set_runtime_status(
                Some(tx),
                RuntimeStatusLevel::Info,
                format!(
                    "current conversation moved to hindsight; runtime conversation, current plan, and events cleared (events={cleared_events}, event_work={cleared_event_work}, telegram_outbox={cleared_outbound}, live_drafts={cleared_live_drafts})"
                ),
            );
            sync_dashboard_state(context, tx, sleep_status, None);
        }
    }
}
