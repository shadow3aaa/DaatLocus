use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::{
    app::{AppStateRender, AppToolExecutionResult, AppToolSpec},
    workspace_app::WorkspaceAppConfigOutput,
};

#[derive(Debug, Deserialize, Serialize)]
pub struct WorkerHello {
    pub token: String,
    pub app_id: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct WorkerRequest {
    pub id: u64,
    pub op: WorkerRequestOp,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkerRequestOp {
    Configure,
    Initialize,
    RenderState,
    ListTools,
    CallTool { name: String, arguments: JsonValue },
    PollNotices,
    Shutdown,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct WorkerResponse {
    pub id: u64,
    pub result: WorkerResponseResult,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum WorkerResponseResult {
    Ok { payload: Box<WorkerResponsePayload> },
    Err { message: String },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum WorkerResponsePayload {
    Config(WorkspaceAppConfigOutput),
    RenderState(AppStateRender),
    ToolSpecs(Vec<AppToolSpec>),
    ToolResult(Box<AppToolExecutionResult>),
    Notice(Option<String>),
    Unit,
}
