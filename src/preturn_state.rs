//! Current state rendered into pre-turn model-facing runtime context.

use std::fmt::Display;

use crate::{
    context::Context, context_budget::truncate_text_to_token_budget, plan::Plan,
    system_info::SystemInfo,
};

const PRETURN_SENSORY_MAX_TOKENS: usize = 400;
const PRETURN_PLAN_MAX_TOKENS: usize = 1_600;
const PRETURN_PLAN_MAX_ITEMS: usize = 8;

/// Current execution state that is injected before each model turn.
pub struct PreTurnState {
    sensory: Sensory,
    plan: Plan,
}

impl PreTurnState {
    pub async fn new(_context: &mut Context) -> Self {
        Self {
            sensory: Sensory::new(),
            plan: _context.plan.clone(),
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
}

impl Display for PreTurnState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Sensory:")?;
        writeln!(f, "{}", self.sensory)?;
        writeln!(f, "Plan:")?;
        write!(f, "{}", self.plan)
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
