//! Current state rendered into pre-turn model-facing runtime context.

use std::fmt::Display;

use crate::{
    app::{AppId, AppStateRender},
    context::Context,
    context_budget::truncate_text_to_token_budget,
    plan::Plan,
    system_info::SystemInfo,
};

const PRETURN_SENSORY_MAX_TOKENS: usize = 400;
const PRETURN_PLAN_MAX_TOKENS: usize = 1_600;
const PRETURN_PLAN_MAX_ITEMS: usize = 8;
const PRETURN_APP_LINES_PER_APP: usize = 8;

/// Current execution state that is injected before each model turn.
pub struct PreTurnState {
    sensory: Sensory,
    plan: Plan,
    apps: AppPreTurnState,
}

#[derive(Clone)]
pub struct PreTurnAppStateEntry {
    pub app_id: String,
    pub title: String,
    pub lines: Vec<String>,
}

impl PreTurnState {
    pub async fn new(context: &mut Context) -> Self {
        let apps = AppPreTurnState::new(context);
        Self {
            sensory: Sensory::new(),
            plan: context.plan.clone(),
            apps,
        }
    }

    pub fn sensory_runtime_text(&self) -> String {
        truncate_text_to_token_budget(&self.sensory.to_string(), PRETURN_SENSORY_MAX_TOKENS)
    }

    pub fn plan_runtime_text(&self) -> String {
        let steps = self.plan.steps();
        if steps.is_empty() {
            return "No current plan.".to_string();
        }

        let omitted = steps.len().saturating_sub(PRETURN_PLAN_MAX_ITEMS);
        let mut lines = Vec::new();
        for (index, step) in steps.iter().take(PRETURN_PLAN_MAX_ITEMS).enumerate() {
            if index > 0 {
                lines.push(String::new());
            }
            lines.push(format!("{}. [{}] {}", index + 1, step.status, step.step));
        }
        if omitted > 0 {
            lines.push(String::new());
            lines.push(format!("... {omitted} more plan item(s) omitted"));
        }
        truncate_text_to_token_budget(&lines.join("\n"), PRETURN_PLAN_MAX_TOKENS)
    }

    pub fn focused_app_runtime_text(&self) -> String {
        self.apps.focused_runtime_text()
    }

    pub fn app_state_entries(&self) -> Vec<PreTurnAppStateEntry> {
        self.apps.app_state_entries(PRETURN_APP_LINES_PER_APP)
    }
}

impl Display for PreTurnState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Sensory:")?;
        writeln!(f, "{}", self.sensory)?;
        writeln!(f, "Plan:")?;
        writeln!(f, "{}", self.plan)?;
        writeln!(f, "Apps:")?;
        write!(f, "{}", self.apps)
    }
}

struct Sensory {
    time: String,
    machine_status: SystemInfo,
}

impl Sensory {
    fn new() -> Self {
        let local = chrono::Local::now();
        let time = local.format("%Y-%m-%d %H:%M:%S %z").to_string();
        let machine_status = SystemInfo::sample();
        Self {
            time,
            machine_status,
        }
    }
}

impl Display for Sensory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Current time: {}", self.time)?;
        write!(f, "Machine status:\n{}", self.machine_status)
    }
}

struct AppPreTurnState {
    focused_app: Option<AppId>,
    states: Vec<(AppId, AppStateRender)>,
}

impl AppPreTurnState {
    fn new(context: &Context) -> Self {
        Self {
            focused_app: context.apps.focused(),
            states: context.apps.state_renders(),
        }
    }
}

impl Display for AppPreTurnState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.focused_app.as_ref() {
            Some(app) => writeln!(f, "Focused app: {app}")?,
            None => writeln!(f, "Focused app: none")?,
        }

        let attention_hints = self
            .states
            .iter()
            .filter(|(id, _)| self.focused_app.as_ref() != Some(id))
            .filter_map(|(id, state)| app_attention_hint(id.clone(), state));
        let attention_hints = attention_hints.collect::<Vec<_>>();
        if !attention_hints.is_empty() {
            writeln!(f, "Background app notices:")?;
            for hint in attention_hints {
                writeln!(f, "- {hint}")?;
            }
        }

        writeln!(f, "App structure state:")?;
        for (id, state) in &self.states {
            writeln!(f, "- {id} / {}：", state.title)?;
            for line in &state.lines {
                writeln!(f, "  {line}")?;
            }
        }
        Ok(())
    }
}

impl AppPreTurnState {
    fn focused_runtime_text(&self) -> String {
        match self.focused_app.as_ref() {
            Some(app) => app.to_string(),
            None => "none".to_string(),
        }
    }

    fn app_state_entries(&self, max_lines_per_device: usize) -> Vec<PreTurnAppStateEntry> {
        let Some(focused) = self.focused_app.as_ref() else {
            return Vec::new();
        };

        self.states
            .iter()
            .filter(|(id, _)| id == focused)
            .map(|(id, state)| {
                let mut lines = state
                    .lines
                    .iter()
                    .take(max_lines_per_device)
                    .cloned()
                    .collect::<Vec<_>>();
                let omitted = state.lines.len().saturating_sub(max_lines_per_device);
                if omitted > 0 {
                    lines.push(format!("... {omitted} more line(s) omitted"));
                }
                PreTurnAppStateEntry {
                    app_id: id.to_string(),
                    title: state.title.clone(),
                    lines,
                }
            })
            .collect()
    }
}

fn app_attention_hint(app_id: AppId, state: &AppStateRender) -> Option<String> {
    if app_id.is_terminal() {
        let session_id = state
            .lines
            .iter()
            .find_map(|line| line.strip_prefix("session="))
            .and_then(|line| line.split_whitespace().next())
            .unwrap_or("unknown");
        if list_field(&state.lines, "unread_sessions").is_empty() {
            None
        } else {
            Some(format!("Terminal session {session_id} has unread output"))
        }
    } else {
        None
    }
}

fn list_field(lines: &[String], key: &str) -> Vec<String> {
    lines
        .iter()
        .find_map(|line| line.strip_prefix(&format!("{key}=")))
        .map(|value| {
            if value == "none" {
                Vec::new()
            } else {
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|item| !item.is_empty())
                    .map(ToString::to_string)
                    .collect()
            }
        })
        .unwrap_or_default()
}
