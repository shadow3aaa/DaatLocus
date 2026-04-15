//! Dashboard status rendering functions extracted from main.rs.

use std::time::Duration;

use crate::{
    app::AppId,
    context::Context,
    dashboard::{DashboardState, render_activity_from_messages},
    reasoning::{
        runtime_review::unread_runtime_review_count,
        trace::unread_runtime_trace_count,
    },
};

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
    pub unread_runtime_review_backlog: usize,
    pub total_runs: usize,
    pub total_consumed_trace_events: usize,
    pub total_consumed_runtime_reviews: usize,
    pub total_runtime_demos: usize,
    pub total_turn_demos: usize,
    pub total_runtime_demo_evaluations: usize,
    pub total_turn_demo_evaluations: usize,
    pub total_runtime_demo_passed: usize,
    pub total_runtime_demo_regressions: usize,
    pub total_runtime_prompt_candidates: usize,
    pub total_runtime_prompt_rollbacks: usize,
    pub total_runtime_prompt_accepts: usize,
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
        .unwrap_or_else(|| context.config.main_model.model_name.clone());
    let focused_app = context
        .apps
        .focused()
        .map(|app| app.to_string())
        .unwrap_or_else(|| "none".to_string());
    let effective_window = context
        .config
        .main_model
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
    if let Ok(backlog) = unread_runtime_review_count().await {
        sleep_status.unread_runtime_review_backlog = backlog;
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

    let totals_lines = vec![
        format!("• Total runs: {}", sleep_status.total_runs),
        format!(
            "• Total consumed trace events: {}",
            sleep_status.total_consumed_trace_events
        ),
        format!(
            "• Total consumed runtime reviews: {}",
            sleep_status.total_consumed_runtime_reviews
        ),
        format!(
            "• Total runtime demos: {}",
            sleep_status.total_runtime_demos
        ),
        format!("• Total turn demos: {}", sleep_status.total_turn_demos),
        format!(
            "• Total runtime demo evaluations: {}",
            sleep_status.total_runtime_demo_evaluations
        ),
        format!(
            "• Total turn demo evaluations: {}",
            sleep_status.total_turn_demo_evaluations
        ),
        format!(
            "• Total runtime demo passes: {}",
            sleep_status.total_runtime_demo_passed
        ),
        format!(
            "• Total runtime demo regressions: {}",
            sleep_status.total_runtime_demo_regressions
        ),
        format!(
            "• Total prompt candidates: {}",
            sleep_status.total_runtime_prompt_candidates
        ),
        format!(
            "• Total prompt accepts: {}",
            sleep_status.total_runtime_prompt_accepts
        ),
        format!(
            "• Total prompt rollbacks: {}",
            sleep_status.total_runtime_prompt_rollbacks
        ),
    ];
    sections.push(format!("Totals\n{}", totals_lines.join("\n")));

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
            "• Current runtime review backlog: {}",
            sleep_status.unread_runtime_review_backlog
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
    let runtime_turn = if context.active_runtime_turn {
        context
            .active_runtime_phase
            .map(|phase| format!("running ({})", phase.label()))
            .unwrap_or_else(|| "running".to_string())
    } else {
        "idle".to_string()
    };
    sections.push(format!(
        "Overview\nRuntime turn: {runtime_turn}\nFocused app: {focused}\nPlans: {active_plans}\nEvents: {active_events}"
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
