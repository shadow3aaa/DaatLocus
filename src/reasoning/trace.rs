use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::{fs::OpenOptions, io::AsyncWriteExt};

use crate::get_spinova_home;

use super::{runtime::PromptRequest, signature::Signature};

const TRACE_FILE_NAME: &str = "reasoning_traces.jsonl";

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

pub async fn append_program_trace(record: ProgramTraceRecord) {
    let path = get_spinova_home().await.join(TRACE_FILE_NAME);
    let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await
    else {
        return;
    };

    let mut line = match serde_json::to_vec(&record) {
        Ok(bytes) => bytes,
        Err(_) => return,
    };
    line.push(b'\n');
    let _ = file.write_all(&line).await;
}

impl ProgramTraceRecord {
    pub fn new(
        origin: TraceOrigin,
        program_name: impl Into<String>,
        attempt: usize,
        signature: Signature,
        request: PromptRequest,
        raw_response: Value,
        parsed_output: Option<Value>,
        deserialization_error: Option<String>,
    ) -> Self {
        Self {
            timestamp_ms: Utc::now().timestamp_millis(),
            origin,
            program_name: program_name.into(),
            attempt,
            signature,
            request,
            raw_response,
            parsed_output,
            deserialization_error,
        }
    }
}
