use serde::{Deserialize, Serialize};

use crate::activity_event::{PlanActivityDescriptor, PlanActivityKind, PlanStepActivityStatus};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanActivityData {
    #[serde(default)]
    pub kind: PlanActivityKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explanation: Option<String>,
    pub steps: Vec<PlanStepActivityData>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanStepActivityData {
    pub status: PlanStepDisplayStatus,
    pub text: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum PlanStepDisplayStatus {
    Pending,
    InProgress,
    Completed,
}

impl From<PlanActivityDescriptor> for PlanActivityData {
    fn from(data: PlanActivityDescriptor) -> Self {
        Self {
            kind: data.kind,
            explanation: data.explanation,
            steps: data
                .steps
                .into_iter()
                .map(|step| PlanStepActivityData {
                    status: match step.status {
                        PlanStepActivityStatus::Pending => PlanStepDisplayStatus::Pending,
                        PlanStepActivityStatus::InProgress => PlanStepDisplayStatus::InProgress,
                        PlanStepActivityStatus::Completed => PlanStepDisplayStatus::Completed,
                    },
                    text: step.text,
                })
                .collect(),
        }
    }
}
