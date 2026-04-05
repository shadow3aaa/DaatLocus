use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::OnceLock;
use tokio::{fs, fs::OpenOptions, io::AsyncWriteExt};

use crate::spinova_paths::spinova_paths;

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

pub struct RuntimeTraceBatch {
    pub records: Vec<ProgramTraceRecord>,
    pub unread_runtime_count: usize,
    pub next_offset: u64,
}

pub async fn append_program_trace(record: ProgramTraceRecord) {
    let trace_io_guard = trace_io_lock().lock().await;
    let path = spinova_paths().await.journal_file(TRACE_FILE_NAME);
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
    drop(trace_io_guard);
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

pub async fn load_runtime_trace_batch() -> miette::Result<RuntimeTraceBatch> {
    let trace_io_guard = trace_io_lock().lock().await;
    let trace_path = spinova_paths().await.journal_file(TRACE_FILE_NAME);
    let bytes = match fs::read(&trace_path).await {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            drop(trace_io_guard);
            return Ok(RuntimeTraceBatch {
                records: Vec::new(),
                unread_runtime_count: 0,
                next_offset: 0,
            });
        }
        Err(err) => {
            return Err(miette::miette!(
                "failed to read reasoning trace file {}: {err}",
                trace_path.display()
            ));
        }
    };
    let slice = &bytes[..];
    let mut offset = 0u64;
    let mut records = Vec::new();
    let mut unread_runtime_count = 0usize;

    for chunk in slice.split_inclusive(|byte| *byte == b'\n') {
        offset += chunk.len() as u64;
        let line = std::str::from_utf8(chunk)
            .map(str::trim)
            .unwrap_or_default();
        if line.is_empty() {
            continue;
        }
        if let Ok(record) = serde_json::from_str::<ProgramTraceRecord>(line)
            && record.origin == TraceOrigin::Runtime
        {
            unread_runtime_count += 1;
            records.push(record);
        }
    }

    let batch = RuntimeTraceBatch {
        records,
        unread_runtime_count,
        next_offset: offset,
    };
    drop(trace_io_guard);
    Ok(batch)
}

pub async fn unread_runtime_trace_count() -> miette::Result<usize> {
    Ok(load_runtime_trace_batch().await?.unread_runtime_count)
}

pub async fn compact_runtime_trace_file(consumed_offset: u64) -> miette::Result<()> {
    let trace_io_guard = trace_io_lock().lock().await;
    let trace_path = spinova_paths().await.journal_file(TRACE_FILE_NAME);
    let bytes = match fs::read(&trace_path).await {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            drop(trace_io_guard);
            return Ok(());
        }
        Err(err) => {
            return Err(miette::miette!(
                "failed to read reasoning trace file {} for compaction: {err}",
                trace_path.display()
            ));
        }
    };
    let keep_from = (consumed_offset as usize).min(bytes.len());
    let remaining = &bytes[keep_from..];
    fs::write(&trace_path, remaining).await.map_err(|err| {
        miette::miette!(
            "failed to rewrite reasoning trace file {} during compaction: {err}",
            trace_path.display()
        )
    })?;
    drop(trace_io_guard);
    Ok(())
}

fn trace_io_lock() -> &'static tokio::sync::Mutex<()> {
    TRACE_IO_LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}
