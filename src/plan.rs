use std::{collections::HashMap, fmt::Display};

use chrono::Utc;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::persistence::{PersistenceStore, read_postcard_optional};

const PLAN_FILE_NAME: &str = "plan";
const LEGACY_PLAN_FILE_NAME: &str = "todo_board";

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct Plan {
    #[serde(default)]
    steps: Vec<PlanStep>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug, JsonSchema)]
pub struct PlanStep {
    pub step: String,
    pub status: PlanStatus,
    #[serde(default)]
    pub created_at_ms: i64,
    #[serde(default)]
    pub last_updated_at_ms: i64,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlanStatus {
    Pending,
    InProgress,
    Completed,
}

impl Default for PlanStatus {
    fn default() -> Self {
        Self::Pending
    }
}

impl Plan {
    pub async fn new() -> Self {
        let persistence = PersistenceStore::runtime().await;
        let primary_path = persistence.memory_file(PLAN_FILE_NAME);
        let legacy_path = persistence.memory_file(LEGACY_PLAN_FILE_NAME);
        if let Some(plan) = read_postcard_optional::<Self>(&primary_path, "plan").await {
            return plan;
        }
        let Some(data) = std::fs::read(&legacy_path).ok() else {
            return Self::default();
        };
        if let Ok(legacy_plan) = postcard::from_bytes::<LegacyPlan>(&data) {
            return legacy_plan.into_plan();
        }
        Self::default()
    }

    pub fn replace(&mut self, mut steps: Vec<PlanStep>) -> bool {
        let now = Utc::now().timestamp_millis();
        for step in &mut steps {
            if step.created_at_ms == 0 {
                step.created_at_ms = now;
            }
            if step.last_updated_at_ms == 0 {
                step.last_updated_at_ms = now;
            }
        }

        if !steps.is_empty()
            && steps
                .iter()
                .all(|step| matches!(step.status, PlanStatus::Completed))
        {
            steps.clear();
        }

        if self.steps == steps {
            return false;
        }

        self.steps = steps;
        true
    }

    pub fn clear(&mut self) -> bool {
        self.replace(Vec::new())
    }

    pub fn steps(&self) -> &[PlanStep] {
        &self.steps
    }

    pub fn active_steps(&self) -> impl Iterator<Item = &PlanStep> {
        self.steps
            .iter()
            .filter(|step| !matches!(step.status, PlanStatus::Completed))
    }

    pub async fn shutdown(self) {
        PersistenceStore::runtime()
            .await
            .write_postcard_memory(PLAN_FILE_NAME, &self)
            .await
            .unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replace_clears_plan_when_all_steps_are_completed() {
        let mut plan = Plan::default();
        let _ = plan.replace(vec![PlanStep {
            step: "step zero".to_string(),
            status: PlanStatus::InProgress,
            created_at_ms: 0,
            last_updated_at_ms: 0,
        }]);

        let changed = plan.replace(vec![
            PlanStep {
                step: "step one".to_string(),
                status: PlanStatus::Completed,
                created_at_ms: 0,
                last_updated_at_ms: 0,
            },
            PlanStep {
                step: "step two".to_string(),
                status: PlanStatus::Completed,
                created_at_ms: 0,
                last_updated_at_ms: 0,
            },
        ]);

        assert!(changed);
        assert!(plan.steps().is_empty());
    }

    #[test]
    fn clear_empties_existing_plan() {
        let mut plan = Plan::default();
        let _ = plan.replace(vec![PlanStep {
            step: "step one".to_string(),
            status: PlanStatus::InProgress,
            created_at_ms: 0,
            last_updated_at_ms: 0,
        }]);

        let changed = plan.clear();

        assert!(changed);
        assert!(plan.steps().is_empty());
    }
}

impl Display for Plan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.steps.is_empty() {
            return write!(f, "No current plan.");
        }

        for (index, step) in self.steps.iter().enumerate() {
            if index > 0 {
                writeln!(f)?;
            }
            writeln!(f, "{}. [{}] {}", index + 1, step.status, step.step)?;
        }
        Ok(())
    }
}

impl Display for PlanStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => write!(f, "pending"),
            Self::InProgress => write!(f, "in_progress"),
            Self::Completed => write!(f, "completed"),
        }
    }
}

#[derive(Deserialize)]
struct LegacyPlan {
    items: HashMap<Uuid, LegacyPlanItem>,
}

#[derive(Deserialize)]
struct LegacyPlanItem {
    #[serde(default)]
    order: u64,
    title: String,
    status: LegacyPlanStatus,
    created_at_ms: i64,
    last_updated_at_ms: i64,
}

#[derive(Deserialize, Clone, Copy)]
enum LegacyPlanStatus {
    Active,
    Blocked,
    Completed,
    Dropped,
}

impl LegacyPlan {
    fn into_plan(self) -> Plan {
        let mut items = self.items.into_iter().collect::<Vec<_>>();
        items.sort_by(|left, right| {
            let left_order = if left.1.order == 0 {
                u64::MAX
            } else {
                left.1.order
            };
            let right_order = if right.1.order == 0 {
                u64::MAX
            } else {
                right.1.order
            };
            left_order
                .cmp(&right_order)
                .then_with(|| left.1.created_at_ms.cmp(&right.1.created_at_ms))
                .then_with(|| left.0.cmp(&right.0))
        });

        let mut steps = items
            .into_iter()
            .filter_map(|(_, item)| match item.status {
                LegacyPlanStatus::Dropped => None,
                LegacyPlanStatus::Completed => Some(PlanStep {
                    step: item.title,
                    status: PlanStatus::Completed,
                    created_at_ms: item.created_at_ms,
                    last_updated_at_ms: item.last_updated_at_ms,
                }),
                LegacyPlanStatus::Active | LegacyPlanStatus::Blocked => Some(PlanStep {
                    step: item.title,
                    status: PlanStatus::Pending,
                    created_at_ms: item.created_at_ms,
                    last_updated_at_ms: item.last_updated_at_ms,
                }),
            })
            .collect::<Vec<_>>();

        if let Some(step) = steps
            .iter_mut()
            .find(|step| !matches!(step.status, PlanStatus::Completed))
        {
            step.status = PlanStatus::InProgress;
        }

        Plan { steps }
    }
}
