//! Dashboard status rendering functions extracted from main.rs.

use std::time::Duration;

use crate::{app::AppId, context::Context, reasoning::trace::unread_runtime_trace_count};

use super::{DashboardState, render_activity_from_messages};

/// Sleep-related constants used in dashboard rendering.
pub const AUTO_SLEEP_IDLE_THRESHOLD: Duration = Duration::from_secs(300);
pub const AUTO_SLEEP_MIN_INTERVAL: Duration = Duration::from_secs(300);
pub const FORCE_SLEEP_TRACE_BACKLOG_THRESHOLD: usize = 128;

/// Status structure for sleep dashboard display.
#[derive(Default)]
pub struct SleepDashboardStatus {
    pub running: bool,
    pub current_trigger: Option<&'static str>,
    pub last_result: Option<String>,
    pub unread_trace_backlog: usize,
    pub total_runs: usize,
    pub total_prompt_consumed_trace_events: usize,
    pub total_failure_patterns: usize,
    pub total_prompt_reflections: usize,
    pub total_prompt_candidates: usize,
    pub total_prompt_candidate_evaluations: usize,
    pub total_prompt_frontier_entries: usize,
    pub latest_prompt_frontier_root_entries: usize,
    pub latest_prompt_frontier_branched_entries: usize,
    pub latest_prompt_frontier_max_generation: usize,
    pub total_bootstrap_demos: usize,
    pub total_stress_cases: usize,
    pub total_instruction_hypotheses: usize,
    pub total_runtime_demos: usize,
    pub total_turn_demos: usize,
    pub total_prompt_system_additions: usize,
    pub total_compiled_prompt_updates: usize,
    pub total_workflow_evidence_run_records: usize,
    pub total_workflow_reflections: usize,
    pub total_workflow_patch_candidates: usize,
    pub total_workflow_merge_candidates: usize,
    pub total_workflow_candidate_evaluations: usize,
    pub total_workflow_frontier_entries: usize,
    pub latest_workflow_frontier_root_entries: usize,
    pub latest_workflow_frontier_branched_entries: usize,
    pub latest_workflow_frontier_max_generation: usize,
    pub total_workflow_patch_applied: usize,
    pub total_workflow_merge_applied: usize,
    pub total_workflow_update_rollbacks: usize,
    pub total_workflow_optimization_rounds: usize,
}

pub fn sync_dashboard_state(
    context: &Context,
    tx: &tokio::sync::watch::Sender<DashboardState>,
    sleep_status: &SleepDashboardStatus,
    last_cycle_elapsed_ms: Option<u128>,
) {
    tx.send_modify(|state| {
        let app_renders = context.apps.state_renders();
        state.focused_app = context.apps.focused();
        state.status_output = render_status_command_output_for_dashboard(context, &app_renders);
        state.sleep_status_output = render_sleep_status_output_for_dashboard(context, sleep_status);
        state.inspect_telegram_output = render_telegram_status_for_dashboard(context);
        state.system_prompt_output = render_system_prompt_output_for_dashboard(context);
        state.app_status_outputs = render_app_status_outputs_for_dashboard(context);
        state.activity_cells = render_activity_for_dashboard(context);
        state.last_cycle_elapsed_ms = last_cycle_elapsed_ms;
        state.footer_context =
            render_dashboard_footer_context(context, state.footer_estimated_input_tokens);
    });
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
        return format!(
            "{}",
            render_footer_context_with_usage(
                &model,
                estimated_input_tokens,
                effective_window,
                &focused_app
            )
        );
    };
    let used = usize::try_from(info.last_token_usage.input_tokens.max(0)).unwrap_or(0);
    let footer_usage = if used > 0 {
        Some((used, false))
    } else {
        estimated_input_tokens.map(|value| (value, true))
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

pub async fn refresh_sleep_backlogs(sleep_status: &mut SleepDashboardStatus) {
    if let Ok(backlog) = unread_runtime_trace_count().await {
        sleep_status.unread_trace_backlog = backlog;
    }
}

pub fn render_sleep_status_output_for_dashboard(
    context: &Context,
    sleep_status: &SleepDashboardStatus,
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

    let prompt_lines = vec![
        format!("• Total runs: {}", sleep_status.total_runs),
        format!(
            "• Total consumed trace events: {}",
            sleep_status.total_prompt_consumed_trace_events
        ),
        format!(
            "• Total failure patterns: {}",
            sleep_status.total_failure_patterns
        ),
        format!(
            "• Total prompt reflections: {}",
            sleep_status.total_prompt_reflections
        ),
        format!(
            "• Total prompt candidates: {}",
            sleep_status.total_prompt_candidates
        ),
        format!(
            "• Total prompt candidate evaluations: {}",
            sleep_status.total_prompt_candidate_evaluations
        ),
        format!(
            "• Total prompt frontier entries: {}",
            sleep_status.total_prompt_frontier_entries
        ),
        format!(
            "• Latest prompt frontier roots/branched/max_generation: {}/{}/{}",
            sleep_status.latest_prompt_frontier_root_entries,
            sleep_status.latest_prompt_frontier_branched_entries,
            sleep_status.latest_prompt_frontier_max_generation
        ),
        format!(
            "• Total bootstrap demos: {}",
            sleep_status.total_bootstrap_demos
        ),
        format!("• Total stress cases: {}", sleep_status.total_stress_cases),
        format!(
            "• Total instruction hypotheses: {}",
            sleep_status.total_instruction_hypotheses
        ),
        format!(
            "• Total runtime demos: {}",
            sleep_status.total_runtime_demos
        ),
        format!("• Total turn demos: {}", sleep_status.total_turn_demos),
        format!(
            "• Total applied system additions: {}",
            sleep_status.total_prompt_system_additions
        ),
        format!(
            "• Total compiled prompt updates: {}",
            sleep_status.total_compiled_prompt_updates
        ),
    ];
    sections.push(format!("Prompt Improvement\n{}", prompt_lines.join("\n")));

    let workflow_lines = vec![
        format!(
            "• Total workflow evidence run records: {}",
            sleep_status.total_workflow_evidence_run_records
        ),
        format!(
            "• Total workflow reflections: {}",
            sleep_status.total_workflow_reflections
        ),
        format!(
            "• Total workflow patch candidates: {}",
            sleep_status.total_workflow_patch_candidates
        ),
        format!(
            "• Total workflow merge candidates: {}",
            sleep_status.total_workflow_merge_candidates
        ),
        format!(
            "• Total workflow candidate evaluations: {}",
            sleep_status.total_workflow_candidate_evaluations
        ),
        format!(
            "• Total workflow frontier entries: {}",
            sleep_status.total_workflow_frontier_entries
        ),
        format!(
            "• Latest workflow frontier roots/branched/max_generation: {}/{}/{}",
            sleep_status.latest_workflow_frontier_root_entries,
            sleep_status.latest_workflow_frontier_branched_entries,
            sleep_status.latest_workflow_frontier_max_generation
        ),
        format!(
            "• Total workflow patch applied: {}",
            sleep_status.total_workflow_patch_applied
        ),
        format!(
            "• Total workflow merge applied: {}",
            sleep_status.total_workflow_merge_applied
        ),
        format!(
            "• Total workflow update rollbacks: {}",
            sleep_status.total_workflow_update_rollbacks
        ),
        format!(
            "• Total workflow optimization rounds: {}",
            sleep_status.total_workflow_optimization_rounds
        ),
    ];
    sections.push(format!(
        "Workflow Improvement\n{}",
        workflow_lines.join("\n")
    ));

    let mut trigger_lines = vec![
        format!(
            "• Force backlog threshold: {} traces",
            FORCE_SLEEP_TRACE_BACKLOG_THRESHOLD
        ),
        format!(
            "• Current trace backlog: {}",
            sleep_status.unread_trace_backlog
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
    let active_events = context.pending_work.pending_count();
    let bound_workflow = context
        .bound_workflow_id
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
        "Overview\nRuntime turn: {runtime_turn}\nFocused app: {focused}\nBound workflow: {bound_workflow}\nPlans: {active_plans}\nEvents: {active_events}"
    ));

    let usage_lines = render_status_usage_lines(context);
    sections.push(format!("Model usage\n{}", usage_lines.join("\n")));

    let plan_lines = render_status_plan_lines(context);
    sections.push(format!("Plan\n{}", plan_lines.join("\n")));

    sections.join("\n\n")
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
    let queued_outbound = chats
        .iter()
        .map(|chat| chat.pending_outbound_count)
        .sum::<usize>();

    let mut lines = vec![
        "Telegram".to_string(),
        "Role: transport / adapter".to_string(),
        format!("Known chats: {}", chats.len()),
        format!("Queued outbound: {queued_outbound}"),
    ];

    if chats.is_empty() {
        lines.push(String::new());
        lines.push("No chats.".to_string());
        return lines.join("\n");
    }

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

    lines.join("\n")
}

pub fn render_activity_for_dashboard(context: &Context) -> Vec<crate::dashboard::ActivityCell> {
    render_activity_from_messages(context.memory.runtime_conversation_messages())
}
