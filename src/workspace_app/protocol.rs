use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::{
    app::{AppDynamicToolResult, AppDynamicToolSpec, AppStateRender},
    workspace_app::WorkspaceAppConfigOutput,
};

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct WorkerHello {
    pub token: String,
    pub app_id: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct WorkerRequest {
    pub id: u64,
    pub op: WorkerRequestOp,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum WorkerRequestOp {
    Configure,
    Initialize,
    RenderState,
    ListTools,
    CallTool { name: String, arguments: JsonValue },
    OnFocus,
    OnBlur,
    PollNotices,
    Shutdown,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct WorkerResponse {
    pub id: u64,
    pub result: WorkerResponseResult,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum WorkerResponseResult {
    Ok { payload: WorkerResponsePayload },
    Err { message: String },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub(crate) enum WorkerResponsePayload {
    Config(WorkspaceAppConfigOutput),
    RenderState(AppStateRender),
    ToolSpecs(Vec<AppDynamicToolSpec>),
    ToolResult(AppDynamicToolResult),
    Notice(Option<String>),
    Unit,
}
