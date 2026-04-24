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
    sleep_status: &mut SleepDashboardStatus,
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
    sleep_status: &mut SleepDashboardStatus,
) -> Option<String> {
    let Some(idle_since) = context.idle_since else {
        return None;
    };
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
    sleep_status: &mut SleepDashboardStatus,
    trigger: SleepTrigger,
    status: &str,
) {
    *sleep_running = true;
    sleep_status.running = true;
    sleep_status.current_trigger = Some(match trigger {
        SleepTrigger::Manual => "manual",
        SleepTrigger::Idle => "automatic",
    });
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
    sleep_status: &mut SleepDashboardStatus,
    result: SleepTaskResult,
) {
    sleep_status.running = false;
    sleep_status.current_trigger = None;
    match result.result {
        Ok(summary) => {
            if let Ok(store) = load_compiled_prompts_only(&context.config).await {
                context.compiled_prompts = store;
            }
            let prefix = match result.trigger {
                SleepTrigger::Manual => "sleep completed",
                SleepTrigger::Idle => "background sleep completed",
            };
            let prompt = &summary.prompt_improvement;
            let workflow = &summary.workflow_improvement;
            sleep_status.total_runs += 1;
            sleep_status.total_prompt_consumed_trace_events += prompt.consumed_trace_events;
            sleep_status.total_failure_patterns += prompt.failure_patterns.len();
            sleep_status.total_prompt_reflections += prompt.prompt_reflections;
            sleep_status.total_prompt_candidates += prompt.prompt_candidates;
            sleep_status.total_prompt_candidate_evaluations += prompt.prompt_candidate_evaluations;
            sleep_status.total_prompt_frontier_entries += prompt.prompt_frontier_entries;
            sleep_status.latest_prompt_frontier_root_entries = prompt.prompt_frontier_root_entries;
            sleep_status.latest_prompt_frontier_branched_entries =
                prompt.prompt_frontier_branched_entries;
            sleep_status.latest_prompt_frontier_max_generation =
                prompt.prompt_frontier_max_generation;
            sleep_status.total_bootstrap_demos += prompt.bootstrap_demos;
            sleep_status.total_stress_cases += prompt.stress_cases;
            sleep_status.total_instruction_hypotheses += prompt.instruction_hypotheses;
            sleep_status.total_runtime_demos += prompt.runtime_demos;
            sleep_status.total_turn_demos += prompt.turn_demos;
            sleep_status.total_prompt_system_additions += prompt.applied_system_additions;
            sleep_status.total_compiled_prompt_updates +=
                usize::from(prompt.compiled_prompt_updated);
            sleep_status.total_workflow_evidence_run_records += workflow.evidence_run_records;
            sleep_status.total_workflow_reflections += workflow.workflow_reflections;
            sleep_status.total_workflow_patch_candidates += workflow.patch_candidates;
            sleep_status.total_workflow_merge_candidates += workflow.merge_candidates;
            sleep_status.total_workflow_candidate_evaluations += workflow.candidate_evaluations;
            sleep_status.total_workflow_frontier_entries += workflow.frontier_entries;
            sleep_status.latest_workflow_frontier_root_entries = workflow.frontier_root_entries;
            sleep_status.latest_workflow_frontier_branched_entries =
                workflow.frontier_branched_entries;
            sleep_status.latest_workflow_frontier_max_generation = workflow.frontier_max_generation;
            sleep_status.total_workflow_patch_applied += workflow.patch_applied;
            sleep_status.total_workflow_merge_applied += workflow.merge_applied;
            sleep_status.total_workflow_update_rollbacks += workflow.update_rollbacks;
            sleep_status.total_workflow_optimization_rounds += workflow.optimization_rounds;
            let summary_text = summarize_sleep_summary(&summary);
            sleep_status.last_result = Some(summary_text.clone());
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
            sleep_status.last_result = Some(err.to_string());
            set_runtime_status(
                Some(tx),
                RuntimeStatusLevel::Error,
                format!("{prefix}: {err}"),
            );
        }
    }
    refresh_sleep_backlogs(sleep_status).await;
    sync_dashboard_state(context, tx, sleep_status, None);
}
