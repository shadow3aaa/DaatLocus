use std::{collections::BTreeMap, sync::OnceLock};

use serde::{Deserialize, Serialize};
use tokio::{fs, fs::OpenOptions, io::AsyncWriteExt};

use crate::{
    reasoning::{episode::EpisodeActionRecord, runtime::PromptMessage},
    spinova_paths::spinova_paths,
};

const RUNTIME_REVIEWS_FILE_NAME: &str = "runtime_reviews.jsonl";
const MAX_SPAN_GAP_MS: i64 = 120_000;
static RUNTIME_REVIEW_IO_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

#[derive(Clone, Serialize, Deserialize)]
pub struct RuntimeTurnRecord {
    pub id: String,
    pub recorded_at_ms: i64,
    pub current_doing: String,
    pub description: String,
    pub observation: String,
    pub actions: Vec<EpisodeActionRecord>,
    pub before_snapshot_text: String,
    pub after_snapshot_text: String,
    #[serde(default)]
    pub history_messages: Vec<PromptMessage>,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}

pub struct RuntimeReviewBatch {
    pub turns: Vec<RuntimeTurnRecord>,
    pub unread_runtime_review_count: usize,
    pub next_offset: u64,
}

#[derive(Clone)]
pub struct RuntimeReviewSpan {
    pub id: String,
    pub turns: Vec<RuntimeTurnRecord>,
}

impl RuntimeReviewSpan {
    pub fn last_turn(&self) -> &RuntimeTurnRecord {
        self.turns
            .last()
            .expect("runtime review span should contain at least one turn")
    }
}

pub async fn append_runtime_turn_record(turn: &RuntimeTurnRecord) {
    let runtime_review_io_guard = runtime_review_io_lock().lock().await;
    let path = spinova_paths().await.journal_file(RUNTIME_REVIEWS_FILE_NAME);
    let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await
    else {
        return;
    };

    let mut line = match serde_json::to_vec(turn) {
        Ok(bytes) => bytes,
        Err(_) => return,
    };
    line.push(b'\n');
    let _ = file.write_all(&line).await;
    drop(runtime_review_io_guard);
}

pub async fn load_runtime_review_batch() -> miette::Result<RuntimeReviewBatch> {
    let runtime_review_io_guard = runtime_review_io_lock().lock().await;
    let path = spinova_paths().await.journal_file(RUNTIME_REVIEWS_FILE_NAME);
    let bytes = match fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            drop(runtime_review_io_guard);
            return Ok(RuntimeReviewBatch {
                turns: Vec::new(),
                unread_runtime_review_count: 0,
                next_offset: 0,
            });
        }
        Err(err) => {
            return Err(miette::miette!(
                "failed to read runtime review file {}: {err}",
                path.display()
            ));
        }
    };

    let mut offset = 0u64;
    let mut turns = Vec::new();
    let mut unread_runtime_review_count = 0usize;
    for chunk in bytes.split_inclusive(|byte| *byte == b'\n') {
        offset += chunk.len() as u64;
        let line = std::str::from_utf8(chunk)
            .map(str::trim)
            .unwrap_or_default();
        if line.is_empty() {
            continue;
        }
        if let Ok(turn) = serde_json::from_str::<RuntimeTurnRecord>(line) {
            unread_runtime_review_count += 1;
            turns.push(turn);
        }
    }

    let batch = RuntimeReviewBatch {
        turns,
        unread_runtime_review_count,
        next_offset: offset,
    };
    drop(runtime_review_io_guard);
    Ok(batch)
}

pub async fn unread_runtime_review_count() -> miette::Result<usize> {
    Ok(load_runtime_review_batch()
        .await?
        .unread_runtime_review_count)
}

pub async fn compact_runtime_review_file(consumed_offset: u64) -> miette::Result<()> {
    let runtime_review_io_guard = runtime_review_io_lock().lock().await;
    let path = spinova_paths().await.journal_file(RUNTIME_REVIEWS_FILE_NAME);
    let bytes = match fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            drop(runtime_review_io_guard);
            return Ok(());
        }
        Err(err) => {
            return Err(miette::miette!(
                "failed to read runtime review file {} for compaction: {err}",
                path.display()
            ));
        }
    };
    let keep_from = (consumed_offset as usize).min(bytes.len());
    let remaining = &bytes[keep_from..];
    fs::write(&path, remaining).await.map_err(|err| {
        miette::miette!(
            "failed to rewrite runtime review file {} during compaction: {err}",
            path.display()
        )
    })?;
    drop(runtime_review_io_guard);
    Ok(())
}

pub fn build_runtime_review_spans(turns: &[RuntimeTurnRecord]) -> Vec<RuntimeReviewSpan> {
    let mut spans = Vec::new();
    for turn in turns.iter().cloned() {
        match spans.last_mut() {
            Some(current) if should_extend_runtime_span(current, &turn) => current.turns.push(turn),
            _ => spans.push(RuntimeReviewSpan {
                id: turn.id.clone(),
                turns: vec![turn],
            }),
        }
    }
    spans
}

fn should_extend_runtime_span(span: &RuntimeReviewSpan, next: &RuntimeTurnRecord) -> bool {
    let last = span.last_turn();
    if next.recorded_at_ms - last.recorded_at_ms > MAX_SPAN_GAP_MS {
        return false;
    }
    if same_metadata(last, next, "item_id") || same_metadata(last, next, "objective") {
        return true;
    }
    if !last.current_doing.trim().is_empty() && last.current_doing == next.current_doing {
        return true;
    }
    let last_action = last_runtime_turn_action(last);
    let next_action = last_runtime_turn_action(next);
    last_action.kind == next_action.kind && last_action.kind != "assistant_message"
}

fn last_runtime_turn_action(turn: &RuntimeTurnRecord) -> &EpisodeActionRecord {
    turn.actions
        .last()
        .expect("runtime review turn should contain at least one action")
}

fn same_metadata(left: &RuntimeTurnRecord, right: &RuntimeTurnRecord, key: &str) -> bool {
    left.metadata
        .get(key)
        .zip(right.metadata.get(key))
        .map(|(left_value, right_value)| !left_value.trim().is_empty() && left_value == right_value)
        .unwrap_or(false)
}

fn runtime_review_io_lock() -> &'static tokio::sync::Mutex<()> {
    RUNTIME_REVIEW_IO_LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}
