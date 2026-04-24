use super::sleep_driver::{SleepTrigger, start_background_sleep};
use super::*;

pub(crate) async fn handle_dashboard_control_command(
    context: &mut Context,
    tx: &tokio::sync::watch::Sender<DashboardState>,
    sleep_result_tx: &tokio::sync::mpsc::UnboundedSender<SleepTaskResult>,
    sleep_running: &mut bool,
    sleep_status: &mut SleepDashboardStatus,
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
            let retain_plan = context.memory.clear_runtime_conversation().await;
            let _ = context.plan.clear();
            for job in retain_plan.jobs {
                if let Err(err) = context.hindsight_retain.enqueue(job) {
                    tracing::error!("failed to enqueue hindsight retain job during clear: {err:?}");
                }
            }
            if retain_plan.must_flush_before_continue || context.memory.retain_backlog_count() > 0 {
                match context.hindsight_retain.flush().await {
                    Ok(()) => context.memory.mark_queued_retained(),
                    Err(err) => {
                        tracing::error!(
                            "failed to flush hindsight retain queue during clear: {err:?}"
                        );
                    }
                }
            }
            set_runtime_status(
                Some(tx),
                RuntimeStatusLevel::Info,
                "current conversation moved to hindsight; runtime conversation history and current plan cleared",
            );
            sync_dashboard_state(context, tx, sleep_status, None);
        }
    }
}
