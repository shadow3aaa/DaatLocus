use super::*;

#[derive(Default)]
pub(super) enum SleepTrigger {
    #[default]
    Manual,
    Idle,
}

pub(crate) struct SleepTaskResult {
    trigger: SleepTrigger,
    result: Result<crate::reasoning::sleep::SleepSummary>,
}

pub(super) async fn maybe_start_forced_sleep(
    context: &mut Context,
    tx: &tokio::sync::watch::Sender<DashboardState>,
    sleep_result_tx: &tokio::sync::mpsc::UnboundedSender<SleepTaskResult>,
    sleep_running: &mut bool,
    sleep_status: &mut SleepStatusSnapshot,
) -> Option<String> {
    if *sleep_running {
        return None;
    }
    let trace_backlog = sleep_status.unread_trace_backlog;
    if trace_backlog < FORCE_SLEEP_TRACE_BACKLOG_THRESHOLD {
        return None;
    }
    let status = format!(
        "backlog too high (traces={}): background sleep started",
        trace_backlog
    );
    start_background_sleep(
        context,
        tx,
        sleep_result_tx,
        sleep_running,
        sleep_status,
        SleepTrigger::Idle,
        &status,
    )
    .await;
    Some(format!(
        "backlog too high (traces={}): background sleep started",
        trace_backlog
    ))
}

pub(super) async fn maybe_start_idle_sleep(
    context: &mut Context,
    tx: &tokio::sync::watch::Sender<DashboardState>,
    sleep_result_tx: &tokio::sync::mpsc::UnboundedSender<SleepTaskResult>,
    sleep_running: &mut bool,
    sleep_status: &mut SleepStatusSnapshot,
) -> Option<String> {
    let idle_since = context.idle_since?;
    if idle_since.elapsed() < AUTO_SLEEP_IDLE_THRESHOLD {
        return None;
    }
    if context
        .last_idle_sleep_at
        .is_some_and(|last| last.elapsed() < AUTO_SLEEP_MIN_INTERVAL)
    {
        return None;
    }
    if *sleep_running {
        return Some("idle: background sleep is running".to_string());
    }
    context.last_idle_sleep_at = Some(std::time::Instant::now());
    start_background_sleep(
        context,
        tx,
        sleep_result_tx,
        sleep_running,
        sleep_status,
        SleepTrigger::Idle,
        "idle: background sleep started",
    )
    .await;
    Some("idle: background sleep started".to_string())
}

pub(super) async fn start_background_sleep(
    context: &mut Context,
    tx: &tokio::sync::watch::Sender<DashboardState>,
    sleep_result_tx: &tokio::sync::mpsc::UnboundedSender<SleepTaskResult>,
    sleep_running: &mut bool,
    sleep_status: &mut SleepStatusSnapshot,
    trigger: SleepTrigger,
    status: &str,
) {
    *sleep_running = true;
    sleep_status.mark_started(match trigger {
        SleepTrigger::Manual => "manual",
        SleepTrigger::Idle => "automatic",
    });
    if let Err(err) = persist_sleep_status_snapshot(sleep_status).await {
        tracing::warn!("failed to persist sleep status start: {err:?}");
    }
    set_runtime_status(Some(tx), RuntimeStatusLevel::Info, status.to_string());
    sync_dashboard_state(context, tx, sleep_status, None);
    let config = context.config.clone();
    let compiled_prompts = context.compiled_prompts.clone();
    let sleep_result_tx = sleep_result_tx.clone();
    tokio::spawn(async move {
        let mut sleep_context = build_eval_context_with_compiled(config, compiled_prompts).await;
        let result = run_sleep(&mut sleep_context).await;
        sleep_context.shutdown().await;
        let _ = sleep_result_tx.send(SleepTaskResult { trigger, result });
    });
}

pub(crate) async fn handle_sleep_task_result(
    context: &mut Context,
    tx: &tokio::sync::watch::Sender<DashboardState>,
    sleep_status: &mut SleepStatusSnapshot,
    result: SleepTaskResult,
) {
    match result.result {
        Ok(summary) => {
            if let Ok(store) = load_compiled_prompts_only(&context.config).await {
                context.compiled_prompts = store;
            }
            let prefix = match result.trigger {
                SleepTrigger::Manual => "sleep completed",
                SleepTrigger::Idle => "background sleep completed",
            };
            sleep_status.apply_summary(&summary);
            let summary_text = summarize_sleep_summary(&summary);
            sleep_status.mark_completed(summary_text.clone());
            set_runtime_status(
                Some(tx),
                RuntimeStatusLevel::Info,
                format!("{prefix}: {summary_text}"),
            );
        }
        Err(err) => {
            let prefix = match result.trigger {
                SleepTrigger::Manual => "sleep failed",
                SleepTrigger::Idle => "background sleep failed",
            };
            sleep_status.mark_completed(err.to_string());
            set_runtime_status(
                Some(tx),
                RuntimeStatusLevel::Error,
                format!("{prefix}: {err}"),
            );
        }
    }
    refresh_sleep_status_queues(sleep_status).await;
    if let Err(err) = persist_sleep_status_snapshot(sleep_status).await {
        tracing::warn!("failed to persist sleep status result: {err:?}");
    }
    sync_dashboard_state(context, tx, sleep_status, None);
}
