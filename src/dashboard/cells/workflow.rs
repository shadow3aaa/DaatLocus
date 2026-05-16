use serde::{Deserialize, Serialize};

use crate::tool_ui::{ActivateWorkflowUiData, CreateWorkflowUiData};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActivateWorkflowActivityCell {
    pub workflow_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CreateWorkflowActivityCell {
    pub workflow_id: String,
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
