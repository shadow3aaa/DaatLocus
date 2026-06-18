use std::sync::OnceLock;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio::fs;
use uuid::Uuid;

use crate::{
    daat_locus_paths::daat_locus_paths,
    persistence::{PersistenceFileMode, append_bytes_durable, write_bytes_atomic},
};

const RUNTIME_ERROR_CASES_FILE_NAME: &str = "runtime_error_cases.jsonl";
static RUNTIME_ERROR_CASE_IO_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeErrorKind {
    MissingFinishAndSend,
    MissingNoticeResolved,
    InvalidToolArgs,
    ToolSchemaError,
    StaleBrowserRef,
    WrongTerminalSessionContinuation,
    PlanContractViolation,
    EventIdMissingOrStale,
    RepeatedIdenticalToolError,
    ContextOverflowAfterRecovery,
    ClaimedInputLeftUnresolved,
    TransportCompletionViolation,
    ModelRequestRepeatedFailure,
    ModelEmptyReasoningOutput,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RuntimeErrorTaskContext {
    pub origin: Option<String>,
    pub event_sources: Vec<String>,
    pub user_request_summary: Option<String>,
    pub claimed_event_ids: Vec<String>,
    pub claimed_app_notices: Vec<String>,
    pub bound_primitive_id: Option<String>,
    pub workflow_origin: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RuntimeErrorRuntimeContext {
    pub phase: Option<String>,
    pub available_tool_names: Vec<String>,
    pub plan_summary: Vec<String>,
    #[serde(default, alias = "compact_snapshot_summary")]
    pub compact_context_summary: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RuntimeErrorActionContext {
    pub assistant_text_summary: Option<String>,
    pub tool_call_summaries: Vec<String>,
    pub tool_result_summaries: Vec<String>,
    pub previous_action_window: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RuntimeErrorObservation {
    pub expected_behavior: String,
    pub actual_behavior: String,
    pub evidence: String,
    pub recoverability: String,
    pub retry_count: usize,
    pub terminal_status: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeErrorCase {
    pub case_id: String,
    pub turn_id: String,
    pub occurred_at_ms: i64,
    pub error_kind: RuntimeErrorKind,
    pub severity: u8,
    pub detected_by: String,
    pub task: RuntimeErrorTaskContext,
    pub runtime: RuntimeErrorRuntimeContext,
    pub action: RuntimeErrorActionContext,
    pub observation: RuntimeErrorObservation,
    #[serde(default)]
    pub contract_refs: Vec<String>,
}

pub struct RuntimeErrorCaseBatch {
    pub cases: Vec<RuntimeErrorCase>,
    pub unread_case_count: usize,
    pub next_offset: u64,
}

pub struct RuntimeErrorCaseParts {
    pub turn_id: String,
    pub error_kind: RuntimeErrorKind,
    pub severity: u8,
    pub detected_by: String,
    pub task: RuntimeErrorTaskContext,
    pub runtime: RuntimeErrorRuntimeContext,
    pub action: RuntimeErrorActionContext,
    pub observation: RuntimeErrorObservation,
    pub contract_refs: Vec<String>,
}

impl RuntimeErrorCase {
    pub fn new(parts: RuntimeErrorCaseParts) -> Self {
        Self {
            case_id: Uuid::new_v4().to_string(),
            turn_id: parts.turn_id,
            occurred_at_ms: Utc::now().timestamp_millis(),
            error_kind: parts.error_kind,
            severity: parts.severity,
            detected_by: parts.detected_by,
            task: parts.task,
            runtime: parts.runtime,
            action: parts.action,
            observation: parts.observation,
            contract_refs: parts.contract_refs,
        }
    }
}

pub async fn append_runtime_error_case(case: RuntimeErrorCase) {
    let _guard = runtime_error_case_io_lock().lock().await;
    let path = daat_locus_paths()
        .await
        .journal_file(RUNTIME_ERROR_CASES_FILE_NAME);
    let mut line = match serde_json::to_vec(&case) {
        Ok(bytes) => bytes,
        Err(err) => {
            tracing::warn!("failed to encode runtime error case: {err}");
            return;
        }
    };
    line.push(b'\n');
    if let Err(err) = append_bytes_durable(path, line).await {
        tracing::warn!("failed to append runtime error case: {err}");
    }
}

pub async fn load_runtime_error_case_batch() -> miette::Result<RuntimeErrorCaseBatch> {
    let _guard = runtime_error_case_io_lock().lock().await;
    let path = daat_locus_paths()
        .await
        .journal_file(RUNTIME_ERROR_CASES_FILE_NAME);
    let bytes = match fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RuntimeErrorCaseBatch {
                cases: Vec::new(),
                unread_case_count: 0,
                next_offset: 0,
            });
        }
        Err(err) => {
            return Err(miette::miette!(
                "failed to read runtime error case file {}: {err}",
                path.display()
            ));
        }
    };

    let mut offset = 0u64;
    let mut cases = Vec::new();
    for chunk in bytes.split_inclusive(|byte| *byte == b'\n') {
        offset += chunk.len() as u64;
        let line = std::str::from_utf8(chunk)
            .map(str::trim)
            .unwrap_or_default();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<RuntimeErrorCase>(line) {
            Ok(case) => cases.push(case),
            Err(err) => tracing::warn!("skipping malformed runtime error case: {err}"),
        }
    }

    Ok(RuntimeErrorCaseBatch {
        unread_case_count: cases.len(),
        cases,
        next_offset: offset,
    })
}

pub async fn unread_runtime_error_case_count() -> miette::Result<usize> {
    Ok(load_runtime_error_case_batch().await?.unread_case_count)
}

pub async fn compact_runtime_error_case_file(consumed_offset: u64) -> miette::Result<()> {
    let _guard = runtime_error_case_io_lock().lock().await;
    let path = daat_locus_paths()
        .await
        .journal_file(RUNTIME_ERROR_CASES_FILE_NAME);
    let bytes = match fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(miette::miette!(
                "failed to read runtime error case file {} for compaction: {err}",
                path.display()
            ));
        }
    };
    let keep_from = (consumed_offset as usize).min(bytes.len());
    write_bytes_atomic(
        path.clone(),
        bytes[keep_from..].to_vec(),
        PersistenceFileMode::Default,
    )
    .await
    .map_err(|err| {
        miette::miette!(
            "failed to rewrite runtime error case file {} during compaction: {err}",
            path.display()
        )
    })
}

fn runtime_error_case_io_lock() -> &'static tokio::sync::Mutex<()> {
    RUNTIME_ERROR_CASE_IO_LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_error_kind_serializes_as_whitelist_value() {
        let value = serde_json::to_value(RuntimeErrorKind::MissingFinishAndSend).unwrap();
        assert_eq!(value, serde_json::json!("missing_finish_and_send"));
    }
}
