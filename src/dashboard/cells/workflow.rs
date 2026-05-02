use serde::{Deserialize, Serialize};

use crate::tool_ui::{ActivateWorkflowUiData, CreateWorkflowUiData, DeepRecallUiData};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActivateWorkflowActivityCell {
    pub workflow_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CreateWorkflowActivityCell {
    pub workflow_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeepRecallActivityCell {
    pub memory_count: usize,
}

impl From<ActivateWorkflowUiData> for ActivateWorkflowActivityCell {
    fn from(data: ActivateWorkflowUiData) -> Self {
        ActivateWorkflowActivityCell {
            workflow_id: data.workflow_id,
        }
    }
}

impl From<CreateWorkflowUiData> for CreateWorkflowActivityCell {
    fn from(data: CreateWorkflowUiData) -> Self {
        CreateWorkflowActivityCell {
            workflow_id: data.workflow_id,
        }
    }
}

impl From<DeepRecallUiData> for DeepRecallActivityCell {
    fn from(data: DeepRecallUiData) -> Self {
        DeepRecallActivityCell {
            memory_count: data.memory_count,
        }
    }
}
