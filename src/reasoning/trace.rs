//! Reasoning trace recording and playback.
//! Some trace functions serve offline evaluation pipelines.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::OnceLock;

use crate::{daat_locus_paths::daat_locus_paths, persistence::append_bytes_durable};

use super::{runtime::PromptRequest, signature::Signature};

const TRACE_FILE_NAME: &str = "reasoning_traces.jsonl";
static TRACE_IO_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceOrigin {
    Runtime,
    Compile,
    Eval,
    Sleep,
    BenchCompile,
    BenchEval,
    #[default]
    Unknown,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ProgramTraceRecord {
    pub timestamp_ms: i64,
    #[serde(default)]
    pub origin: TraceOrigin,
    pub program_name: String,
    pub attempt: usize,
    pub signature: Signature,
    pub request: PromptRequest,
    pub raw_response: Value,
    pub parsed_output: Option<Value>,
    pub deserialization_error: Option<String>,
}

pub struct ProgramTraceRecordParts {
    pub origin: TraceOrigin,
    pub program_name: String,
    pub attempt: usize,
    pub signature: Signature,
    pub request: PromptRequest,
    pub raw_response: Value,
    pub parsed_output: Option<Value>,
    pub deserialization_error: Option<String>,
}

pub async fn append_program_trace(record: ProgramTraceRecord) {
    let trace_io_guard = trace_io_lock().lock().await;
    let path = daat_locus_paths().await.journal_file(TRACE_FILE_NAME);
    let mut line = match serde_json::to_vec(&record) {
        Ok(bytes) => bytes,
        Err(_) => return,
    };
    line.push(b'\n');
    let _ = append_bytes_durable(path, line).await;
    drop(trace_io_guard);
}

impl ProgramTraceRecord {
    pub fn new(parts: ProgramTraceRecordParts) -> Self {
        let ProgramTraceRecordParts {
            origin,
            program_name,
            attempt,
            signature,
            request,
            raw_response,
            parsed_output,
            deserialization_error,
        } = parts;

        Self {
            timestamp_ms: Utc::now().timestamp_millis(),
            origin,
            program_name,
            attempt,
            signature,
            request,
            raw_response,
            parsed_output,
            deserialization_error,
        }
    }
}

fn trace_io_lock() -> &'static tokio::sync::Mutex<()> {
    TRACE_IO_LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}
