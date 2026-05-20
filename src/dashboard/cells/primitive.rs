use serde::{Deserialize, Serialize};

use crate::tool_ui::{ActivatePrimitiveUiData, CreatePrimitiveSpecUiData};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActivatePrimitiveActivityCell {
    pub primitive_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CreatePrimitiveSpecActivityCell {
    pub primitive_id: String,
}

impl From<ActivatePrimitiveUiData> for ActivatePrimitiveActivityCell {
    fn from(data: ActivatePrimitiveUiData) -> Self {
        ActivatePrimitiveActivityCell {
            primitive_id: data.primitive_id,
        }
    }
}

impl From<CreatePrimitiveSpecUiData> for CreatePrimitiveSpecActivityCell {
    fn from(data: CreatePrimitiveSpecUiData) -> Self {
        CreatePrimitiveSpecActivityCell {
            primitive_id: data.primitive_id,
        }
    }
}
