//! Dashboard status rendering functions extracted from main.rs.

use std::time::Duration;

use crate::{
    app::AppId,
    context::Context,
    core::TokenUsageInfo,
    events::EventStatus,
    plan::{PlanStatus, PlanStep},
    sleep_status::SleepStatusSnapshot,
};

use super::{
    DashboardContextCompositionSnapshot, DashboardPlanStep, DashboardPrimitiveOptimizationSnapshot,
    DashboardRuntimeActivity, DashboardRuntimeActivityStatus, DashboardRuntimeOptimizationSnapshot,
    DashboardRuntimeStatusLevel, DashboardState, DashboardTokenUsageSnapshot,
    activity_cells_from_history_items, dashboard_agent_name, render_activity_from_messages,
};

/// Sleep-related constants used in dashboard rendering.
pub const AUTO_SLEEP_IDLE_THRESHOLD: Duration = Duration::from_secs(300);
pub const AUTO_SLEEP_MIN_INTERVAL: Duration = Duration::from_secs(300);
pub const FORCE_SLEEP_ERROR_BACKLOG_THRESHOLD: usize = 128;

pub fn sync_dashboard_state(
    context: &Context,
    tx: &tokio::sync::watch::Sender<DashboardState>,
    sleep_status: &SleepStatusSnapshot,
    last_cycle_elapsed_ms: Option<u128>,
) {
    tx.send_modify(|state| {
        let app_renders = context.apps.state_renders();
        state.agent_name = dashboard_agent_name();
        state.session_title = context.session_title.snapshot();
        state.focused_app = context.apps.focused();
        state.status_output = render_status_command_output_for_dashboard(context, &app_renders);
        state.sleep_status_output = render_sleep_status_output_for_dashboard(context, sleep_status);
        state.inspect_telegram_output = render_telegram_status_for_dashboard(context);
        state.system_prompt_output = render_system_prompt_output_for_dashboard(context);
        state.app_status_outputs = render_app_status_outputs_for_dashboard(context);
        state.pending_access_requests = context.telegram_acl.pending_requests();
        state.activity_cells = if state.activity_history.items.is_empty() {
            render_activity_for_dashboard(context)
        } else {
            activity_cells_from_history_items(&state.activity_history.items)
        };
        crate::dashboard::sync_web_activity_state(state);
        state.last_cycle_elapsed_ms = last_cycle_elapsed_ms.map(duration_millis_to_u64);
        state.runtime_activity = runtime_activity_for_dashboard(
            context,
            sleep_status,
            state.runtime_status.as_deref(),
            state.runtime_status_level,
        );
        state.footer_context =
            render_dashboard_footer_context(context, state.footer_estimated_input_tokens);
        state.current_plan_step = current_plan_step_for_dashboard(context);
        state.token_usage = token_usage_snapshot_for_dashboard(context);
        state.runtime_optimization = runtime_optimization_snapshot_for_dashboard(sleep_status);
        state.primitive_optimization = primitive_optimization_snapshot_for_dashboard(sleep_status);
        state.context_composition = context_composition_snapshot_for_dashboard(context);
    });
}

pub fn current_plan_step_for_dashboard(context: &Context) -> Option<DashboardPlanStep> {
    let step = context
        .plan
        .steps()
        .iter()
        .find(|step| matches!(step.status, PlanStatus::InProgress))
        .or_else(|| {
            context
                .plan
                .steps()
                .iter()
                .find(|step| matches!(step.status, PlanStatus::Pending))
        })?;

    Some(DashboardPlanStep {
        status: dashboard_plan_status(step),
        step: step.step.clone(),
    })
}

fn dashboard_plan_status(step: &PlanStep) -> String {
    match step.status {
        PlanStatus::Pending => "pending",
        PlanStatus::InProgress => "in_progress",
        PlanStatus::Completed => "completed",
    }
    .to_string()
}

pub fn token_usage_snapshot_for_dashboard(context: &Context) -> DashboardTokenUsageSnapshot {
    DashboardTokenUsageSnapshot {
        main: visible_token_usage(context.llm.token_usage_info()),
        main_model: context
            .llm
            .model_name()
            .or_else(|| Some(context.config.main_model_config().model_id.clone())),
        judge: visible_token_usage(context.judge_llm.token_usage_info()),
        judge_model: context
            .judge_llm
            .model_name()
            .or_else(|| Some(context.config.judge_model_config().model_id.clone())),
        efficient_model: Some(context.config.efficient_model_config().model_id.clone()),
    }
}

fn visible_token_usage(info: Option<TokenUsageInfo>) -> Option<TokenUsageInfo> {
    info.filter(|info| {
        !info.total_token_usage.is_zero()
            || !info.last_token_usage.is_zero()
            || !info.daily_token_usage.is_empty()
    })
}

pub fn context_composition_snapshot_for_dashboard(
    context: &Context,
) -> Option<DashboardContextCompositionSnapshot> {
    context.latest_context_composition.clone()
}

pub fn runtime_optimization_snapshot_for_dashboard(
    sleep_status: &SleepStatusSnapshot,
) -> DashboardRuntimeOptimizationSnapshot {
    DashboardRuntimeOptimizationSnapshot {
        running: sleep_status.running,
        current_trigger: sleep_status.current_trigger.map(str::to_string),
        last_result: sleep_status.last_result.clone(),
        last_completed_at_ms: sleep_status.last_completed_at_ms,
        unread_runtime_error_backlog: sleep_status.unread_runtime_error_backlog,
        total_runtime_error_cases_consumed: sleep_status.total_runtime_error_cases_consumed,
        total_runtime_error_cases: sleep_status.total_runtime_error_cases,
        total_runtime_error_reflections: sleep_status.total_runtime_error_reflections,
        total_runtime_contract_candidates: sleep_status.total_runtime_contract_candidates,
        total_runtime_contract_candidate_evaluations: sleep_status
            .total_runtime_contract_candidate_evaluations,
        total_runtime_contract_system_additions: sleep_status
            .total_runtime_contract_system_additions,
        total_runtime_contract_updates: sleep_status.total_runtime_contract_updates,
    }
}

pub fn primitive_optimization_snapshot_for_dashboard(
    sleep_status: &SleepStatusSnapshot,
) -> DashboardPrimitiveOptimizationSnapshot {
    DashboardPrimitiveOptimizationSnapshot {
        running: sleep_status.running,
        current_trigger: sleep_status.current_trigger.map(str::to_string),
        last_result: sleep_status.last_result.clone(),
        last_completed_at_ms: sleep_status.last_completed_at_ms,
        primitive_evidence_records: sleep_status.primitive_evidence_records,
        total_primitive_evidence_run_records: sleep_status.total_primitive_evidence_run_records,
        total_primitive_reflections: sleep_status.total_primitive_reflections,
        total_primitive_patch_candidates: sleep_status.total_primitive_patch_candidates,
        total_primitive_merge_candidates: sleep_status.total_primitive_merge_candidates,
        total_primitive_candidate_evaluations: sleep_status.total_primitive_candidate_evaluations,
        total_primitive_frontier_entries: sleep_status.total_primitive_frontier_entries,
        latest_primitive_frontier_root_entries: sleep_status.latest_primitive_frontier_root_entries,
        latest_primitive_frontier_branched_entries: sleep_status
            .latest_primitive_frontier_branched_entries,
        latest_primitive_frontier_max_generation: sleep_status
            .latest_primitive_frontier_max_generation,
        total_primitive_patch_applied: sleep_status.total_primitive_patch_applied,
        total_primitive_merge_applied: sleep_status.total_primitive_merge_applied,
        total_primitive_update_rollbacks: sleep_status.total_primitive_update_rollbacks,
        total_primitive_optimization_rounds: sleep_status.total_primitive_optimization_rounds,
    }
}

pub fn render_dashboard_footer_context(
    context: &Context,
    estimated_input_tokens: Option<usize>,
) -> String {
    let model = context
        .llm
        .model_name()
        .unwrap_or_else(|| context.config.main_model_config().model_id.clone());
    let focused_app = context
        .apps
        .focused()
        .map(|app| app.to_string())
        .unwrap_or_else(|| "none".to_string());
    let effective_window = context
        .config
        .main_model_config()
        .effective_context_window_tokens()
        .max(1);
    let Some(info) = context.llm.token_usage_info() else {
        return render_footer_context_with_usage(
            &model,
            estimated_input_tokens,
            effective_window,
            &focused_app,
        )
        .to_string();
    };
    let used = usize::try_from(info.last_token_usage.input_tokens.max(0)).unwrap_or(0);
    let calibrated = estimated_input_tokens.map(|est| {
        context
            .token_estimate_baseline
            .calibrated_total_input_tokens(est)
    });
    let footer_usage = if used > 0 {
        Some((used, false))
    } else {
        calibrated
            .or(estimated_input_tokens)
            .map(|value| (value, true))
    };
    match footer_usage {
        Some((used, estimated)) => format!(
            "{model} · {}{}/{} used · {}",
            if estimated { "~" } else { "" },
            format_compact_tokens(used),
            format_compact_tokens(effective_window),
            focused_app
        ),
        None => format!(
            "{model} · {} window · {}",
            format_compact_tokens(effective_window),
            focused_app
        ),
    }
}

pub fn render_system_prompt_output_for_dashboard(context: &Context) -> String {
    crate::reasoning::prompt_renderer::DashboardPromptRenderer::render_document(
        &context.runtime_system_prompt_doc(),
        "Runtime System Prompt",
    )
}

pub fn render_app_status_outputs_for_dashboard(context: &Context) -> Vec<(String, String)> {
    let focused = context.apps.focused();
    context
        .apps
        .state_renders()
        .into_iter()
        .map(|(app_id, state)| {
            let usage = context.apps.usage(&app_id).unwrap_or(crate::app::AppUsage {
                description: "No usage available.".to_string(),
                when_to_focus: Vec::new(),
                body_markdown: None,
            });
            let how_to_use = context
                .apps
                .how_to_use(&app_id)
                .unwrap_or(crate::app::AppHowToUse {
                    lines: Vec::new(),
                    body_markdown: None,
                });
            let mut lines = Vec::new();
            let key = app_id.to_string().to_ascii_lowercase();
            lines.push(format!("App Status: {}", state.title));
            lines.push(String::new());
            lines.push("[structured_state]".to_string());
            lines.push(format!("app_id={key}"));
            lines.push(format!("title={}", state.title));
            lines.extend(state.lines.iter().cloned());
            lines.push(String::new());
            lines.push("[usage]".to_string());
            lines.push(crate::reasoning::prompts::build_app_usage_prompt(
                app_id.clone(),
                &usage,
            ));
            lines.push(String::new());
            lines.push("[how_to_use]".to_string());
            if focused.as_ref() == Some(&app_id) {
                lines.push(crate::reasoning::prompts::build_app_how_to_use_prompt(
                    app_id.clone(),
                    &how_to_use,
                ));
            } else {
                lines.push(crate::reasoning::prompts::build_app_pre_focus_note_prompt(
                    app_id.clone(),
                    &state,
                ));
            }
            (key, lines.join("\n"))
        })
        .collect()
}

fn render_footer_context_with_usage(
    model: &str,
    estimated_input_tokens: Option<usize>,
    effective_window: usize,
    focused_app: &str,
) -> String {
    match estimated_input_tokens {
        Some(used) => format!(
            "{model} · ~{}/{} used · {}",
            format_compact_tokens(used),
            format_compact_tokens(effective_window),
            focused_app
        ),
        None => format!(
            "{model} · {} window · {}",
            format_compact_tokens(effective_window),
            focused_app
        ),
    }
}

fn format_compact_tokens(tokens: usize) -> String {
    if tokens >= 1_000_000 {
        let major = tokens / 1_000_000;
        let minor = (tokens % 1_000_000) / 100_000;
        if minor == 0 {
            format!("{major}m")
        } else {
            format!("{major}.{minor}m")
        }
    } else if tokens >= 1_000 {
        let major = tokens / 1_000;
        let minor = (tokens % 1_000) / 100;
        if minor == 0 {
            format!("{major}k")
        } else {
            format!("{major}.{minor}k")
        }
    } else {
        tokens.to_string()
    }
}

pub fn render_sleep_status_output_for_dashboard(
    context: &Context,
    sleep_status: &SleepStatusSnapshot,
) -> String {
    let mut sections = Vec::new();
    let state = if sleep_status.running {
        "running"
    } else {
        "idle"
    };
    let mut overview_lines = vec![format!("State: {state}")];
    if let Some(trigger) = sleep_status.current_trigger {
        overview_lines.push(format!("Trigger: {trigger}"));
    }
    if let Some(last_result) = sleep_status.last_result.as_deref() {
        overview_lines.push(format!("Last result: {last_result}"));
    }
    sections.push(format!("Overview\n{}", overview_lines.join("\n")));

    let queue_lines = [
        format!(
            "• Runtime error queue: {}",
            sleep_status.unread_runtime_error_backlog
        ),
        format!(
            "• Workflow evidence records: {}",
            sleep_status.primitive_evidence_records
        ),
    ];
    sections.push(format!("Queues\n{}", queue_lines.join("\n")));

    let runtime_error_lines = [
        format!("• Total runs: {}", sleep_status.total_runs),
        format!(
            "• Total consumed error cases: {}",
            sleep_status.total_runtime_error_cases_consumed
        ),
        format!(
            "• Total runtime error cases: {}",
            sleep_status.total_runtime_error_cases
        ),
        format!(
            "• Total runtime error reflections: {}",
            sleep_status.total_runtime_error_reflections
        ),
        format!(
            "• Total runtime contract candidates: {}",
            sleep_status.total_runtime_contract_candidates
        ),
        format!(
            "• Total runtime contract candidate evaluations: {}",
            sleep_status.total_runtime_contract_candidate_evaluations
        ),
        format!(
            "• Total runtime contract additions: {}",
            sleep_status.total_runtime_contract_system_additions
        ),
        format!(
            "• Total runtime contract updates: {}",
            sleep_status.total_runtime_contract_updates
        ),
    ];
    sections.push(format!(
        "Runtime Error Correction Totals\n{}",
        runtime_error_lines.join("\n")
    ));

    let primitive_lines = [
        format!(
            "• Total primitive evidence run records: {}",
            sleep_status.total_primitive_evidence_run_records
        ),
        format!(
            "• Total primitive reflections: {}",
            sleep_status.total_primitive_reflections
        ),
        format!(
            "• Total primitive patch candidates: {}",
            sleep_status.total_primitive_patch_candidates
        ),
        format!(
            "• Total primitive merge candidates: {}",
            sleep_status.total_primitive_merge_candidates
        ),
        format!(
            "• Total primitive candidate evaluations: {}",
            sleep_status.total_primitive_candidate_evaluations
        ),
        format!(
            "• Total primitive frontier entries: {}",
            sleep_status.total_primitive_frontier_entries
        ),
        format!(
            "• Latest primitive frontier roots/branched/max_generation: {}/{}/{}",
            sleep_status.latest_primitive_frontier_root_entries,
            sleep_status.latest_primitive_frontier_branched_entries,
            sleep_status.latest_primitive_frontier_max_generation
        ),
        format!(
            "• Total primitive patch applied: {}",
            sleep_status.total_primitive_patch_applied
        ),
        format!(
            "• Total primitive merge applied: {}",
            sleep_status.total_primitive_merge_applied
        ),
        format!(
            "• Total primitive update rollbacks: {}",
            sleep_status.total_primitive_update_rollbacks
        ),
        format!(
            "• Total primitive optimization rounds: {}",
            sleep_status.total_primitive_optimization_rounds
        ),
    ];
    sections.push(format!(
        "Workflow Improvement Totals\n{}",
        primitive_lines.join("\n")
    ));

    let mut trigger_lines = vec![
        format!(
            "• Force backlog threshold: {} runtime errors",
            FORCE_SLEEP_ERROR_BACKLOG_THRESHOLD
        ),
        format!(
            "• Current runtime error queue: {}",
            sleep_status.unread_runtime_error_backlog
        ),
        format!(
            "• Auto sleep after idle: {}",
            format_duration(AUTO_SLEEP_IDLE_THRESHOLD)
        ),
        format!(
            "• Minimum idle sleep interval: {}",
            format_duration(AUTO_SLEEP_MIN_INTERVAL)
        ),
    ];
    match context.idle_since {
        Some(idle_since) => trigger_lines.push(format!(
            "• Currently idle for {}",
            format_duration(idle_since.elapsed())
        )),
        None => trigger_lines.push("• Currently not idle".to_string()),
    }
    if let Some(last_idle_sleep_at) = context.last_idle_sleep_at {
        trigger_lines.push(format!(
            "• Last idle sleep: {} ago",
            format_duration(last_idle_sleep_at.elapsed())
        ));
    }
    sections.push(format!("Triggers\n{}", trigger_lines.join("\n")));

    sections.join("\n\n")
}

fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    if seconds >= 3600 {
        let hours = seconds / 3600;
        let minutes = (seconds % 3600) / 60;
        if minutes == 0 {
            format!("{hours}h")
        } else {
            format!("{hours}h {minutes}m")
        }
    } else if seconds >= 60 {
        let minutes = seconds / 60;
        let rem = seconds % 60;
        if rem == 0 {
            format!("{minutes}m")
        } else {
            format!("{minutes}m {rem}s")
        }
    } else {
        format!("{seconds}s")
    }
}

fn duration_millis_to_u64(ms: u128) -> u64 {
    u64::try_from(ms).unwrap_or(u64::MAX)
}

pub fn runtime_activity_for_dashboard(
    context: &Context,
    sleep_status: &SleepStatusSnapshot,
    runtime_status: Option<&str>,
    runtime_status_level: Option<DashboardRuntimeStatusLevel>,
) -> DashboardRuntimeActivity {
    if runtime_status_level == Some(DashboardRuntimeStatusLevel::Error) {
        return DashboardRuntimeActivity::new(
            DashboardRuntimeActivityStatus::Error,
            "Error",
            runtime_status.map(str::to_string),
        );
    }

    let active_runtime_phase = context
        .active_runtime_phase
        .map(|phase| phase.label().to_string());

    if context.active_runtime_turn {
        let status = match context.active_runtime_phase {
            Some(crate::context::RuntimeTurnPhase::PreflightPreTurnContext)
            | Some(crate::context::RuntimeTurnPhase::PreflightCompaction)
            | Some(crate::context::RuntimeTurnPhase::ModelRequest) => {
                DashboardRuntimeActivityStatus::Thinking
            }
            Some(crate::context::RuntimeTurnPhase::ToolExecution) => {
                DashboardRuntimeActivityStatus::Tooling
            }
            None => DashboardRuntimeActivityStatus::Running,
        };
        let label = match status {
            DashboardRuntimeActivityStatus::Thinking => "Thinking",
            DashboardRuntimeActivityStatus::Tooling => "Using tools",
            _ => "Running",
        };
        return DashboardRuntimeActivity::new(status, label, runtime_status.map(str::to_string))
            .with_runtime_turn(active_runtime_phase);
    }

    if sleep_status.running {
        return DashboardRuntimeActivity::new(
            DashboardRuntimeActivityStatus::Waiting,
            "Waiting",
            runtime_status
                .or(Some("Sleep is running"))
                .map(str::to_string),
        );
    }

    match runtime_status_level {
        Some(DashboardRuntimeStatusLevel::Debug)
        | Some(DashboardRuntimeStatusLevel::Info)
        | Some(DashboardRuntimeStatusLevel::Warn) => DashboardRuntimeActivity::new(
            DashboardRuntimeActivityStatus::Running,
            "Running",
            runtime_status.and_then(trimmed_runtime_status_detail),
        ),
        _ => DashboardRuntimeActivity::default(),
    }
}

fn trimmed_runtime_status_detail(status: &str) -> Option<String> {
    let status = status.trim();
    (!status.is_empty()).then(|| status.to_string())
}

pub fn render_status_command_output_for_dashboard(
    context: &Context,
    _: &[(AppId, crate::app::AppStateRender)],
) -> String {
    let mut sections = Vec::new();

    let focused = context
        .apps
        .focused()
        .map(|app| app.to_string())
        .unwrap_or_else(|| "none".to_string());
    let active_plans = context.plan.active_steps().count();
    let event_summary = render_status_event_summary(context);
    let bound_primitive = context
        .bound_primitive_id
        .clone()
        .unwrap_or_else(|| "none".to_string());
    let runtime_turn = if context.active_runtime_turn {
        context
            .active_runtime_phase
            .map(|phase| format!("running ({})", phase.label()))
            .unwrap_or_else(|| "running".to_string())
    } else {
        "idle".to_string()
    };
    sections.push(format!(
        "Overview\nRuntime turn: {runtime_turn}\nFocused app: {focused}\nBound primitive: {bound_primitive}\nPlans: {active_plans}\nEvents: {event_summary}"
    ));

    let usage_lines = render_status_usage_lines(context);
    sections.push(format!("Model usage\n{}", usage_lines.join("\n")));

    let plan_lines = render_status_plan_lines(context);
    sections.push(format!("Plan\n{}", plan_lines.join("\n")));

    sections.join("\n\n")
}

fn render_status_event_summary(context: &Context) -> String {
    render_status_event_summary_from_statuses(
        context
            .events
            .driver_event_statuses()
            .into_iter()
            .map(|(_, status)| status),
    )
}

fn render_status_event_summary_from_statuses(
    statuses: impl IntoIterator<Item = EventStatus>,
) -> String {
    let mut pending = 0usize;
    let mut claimed = 0usize;
    let mut awaiting_delivery = 0usize;
    let mut failed = 0usize;

    for status in statuses {
        match status {
            EventStatus::Pending => pending += 1,
            EventStatus::Claimed => claimed += 1,
            EventStatus::AwaitingDelivery => awaiting_delivery += 1,
            EventStatus::Failed => failed += 1,
            EventStatus::Resolved | EventStatus::Dismissed => {}
        }
    }

    let active = pending + claimed + awaiting_delivery + failed;
    if active == 0 {
        return "0".to_string();
    }

    let mut parts = Vec::new();
    if pending > 0 {
        parts.push(format!("pending={pending}"));
    }
    if claimed > 0 {
        parts.push(format!("claimed={claimed}"));
    }
    if awaiting_delivery > 0 {
        parts.push(format!("awaiting_delivery={awaiting_delivery}"));
    }
    if failed > 0 {
        parts.push(format!("failed={failed}"));
    }
    format!("{active} active ({})", parts.join(", "))
}

fn render_status_usage_lines(context: &Context) -> Vec<String> {
    let mut lines = Vec::new();
    for (label, llm) in [("main", &context.llm), ("judge", &context.judge_llm)] {
        let Some(info) = llm.token_usage_info() else {
            continue;
        };
        if info.total_token_usage.is_zero() {
            continue;
        }
        let model = llm.model_name().unwrap_or_else(|| "<unknown>".to_string());
        let context_window = info
            .model_context_window
            .map(|value| value.to_string())
            .unwrap_or_else(|| "?".to_string());
        lines.push(format!(
            "• {label}  model={model} total={} input={} output={} cached={} reasoning={} window={context_window}",
            info.total_token_usage.total_tokens,
            info.total_token_usage.input_tokens,
            info.total_token_usage.output_tokens,
            info.total_token_usage.cached_input_tokens,
            info.total_token_usage.reasoning_output_tokens,
        ));
        lines.push(format!(
            "  last={} input={} output={}",
            info.last_token_usage.total_tokens,
            info.last_token_usage.input_tokens,
            info.last_token_usage.output_tokens,
        ));
    }
    if lines.is_empty() {
        vec!["No token usage recorded yet.".to_string()]
    } else {
        lines
    }
}

fn render_status_plan_lines(context: &Context) -> Vec<String> {
    let steps = context.plan.steps();
    if steps.is_empty() {
        return vec!["No active plan items.".to_string()];
    }
    steps
        .iter()
        .take(6)
        .map(|step| format!("• {}  [{}]", step.step, step.status))
        .collect()
}

pub fn render_telegram_status_for_dashboard(context: &Context) -> String {
    let chats = context.telegram.chat_summaries_view();
    let pending_requests = context.telegram_acl.pending_requests();
    let queued_outbound = chats
        .iter()
        .map(|chat| chat.pending_outbound_count)
        .sum::<usize>();

    let mut lines = vec![
        "Telegram".to_string(),
        "Role: transport / adapter".to_string(),
        format!("Known chats: {}", chats.len()),
        format!("Pending approvals: {}", pending_requests.len()),
        format!("Queued outbound: {queued_outbound}"),
    ];

    if chats.is_empty() && pending_requests.is_empty() {
        lines.push(String::new());
        lines.push("No chats or pending approvals.".to_string());
        return lines.join("\n");
    }

    if !pending_requests.is_empty() {
        lines.push(String::new());
        lines.push("Pending approval requests".to_string());
        lines.extend(
            pending_requests
                .iter()
                .take(8)
                .enumerate()
                .map(|(index, request)| {
                    format!(
                        "{}. {} ({}) from {} :: {}",
                        index + 1,
                        request.title,
                        request.chat_id,
                        request.sender,
                        request.last_message_preview
                    )
                }),
        );
    }

    if !chats.is_empty() {
        lines.push(String::new());
        lines.push("Chats".to_string());
        lines.extend(chats.iter().take(8).map(|chat| {
            let mut flags = Vec::new();
            if chat.pending_outbound_count > 0 {
                flags.push(format!("{} queued", chat.pending_outbound_count));
            }
            let suffix = if flags.is_empty() {
                String::new()
            } else {
                format!("  [{}]", flags.join(", "))
            };
            format!("• {} ({}){}", chat.title, chat.chat_id, suffix)
        }));
    }

    lines.join("\n")
}

pub fn render_activity_for_dashboard(context: &Context) -> Vec<crate::dashboard::ActivityCell> {
    render_activity_from_messages(context.memory.runtime_conversation_messages())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_summary_counts_claimed_events_as_active() {
        assert_eq!(
            render_status_event_summary_from_statuses([EventStatus::Claimed]),
            "1 active (claimed=1)"
        );
    }

    #[test]
    fn event_summary_reports_active_event_states() {
        assert_eq!(
            render_status_event_summary_from_statuses([
                EventStatus::Pending,
                EventStatus::Claimed,
                EventStatus::AwaitingDelivery,
                EventStatus::Failed,
                EventStatus::Resolved,
                EventStatus::Dismissed,
            ]),
            "4 active (pending=1, claimed=1, awaiting_delivery=1, failed=1)"
        );
    }

    #[test]
    fn event_summary_ignores_terminal_success_states() {
        assert_eq!(
            render_status_event_summary_from_statuses([
                EventStatus::Resolved,
                EventStatus::Dismissed,
            ]),
            "0"
        );
    }
}
