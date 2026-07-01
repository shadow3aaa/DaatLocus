use std::{
    collections::HashSet,
    sync::OnceLock,
};

use miette::{Result, miette};
use serde::{Deserialize, Serialize};
use tokio::{
    fs::{self, OpenOptions},
    io::{AsyncWriteExt, BufReader, AsyncBufReadExt},
};

use crate::daat_locus_paths::daat_locus_paths;

const SKILL_RUN_RECORDS_DIR: &str = "skills";
const SKILL_RUN_RECORDS_FILE: &str = "run_records.jsonl";

static SKILL_RUN_RECORDS_IO_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

fn skill_run_records_io_lock() -> &'static tokio::sync::Mutex<()> {
    SKILL_RUN_RECORDS_IO_LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

async fn skill_run_records_file_path() -> std::path::PathBuf {
    daat_locus_paths()
        .await
        .runtime_dir()
        .join(SKILL_RUN_RECORDS_DIR)
        .join(SKILL_RUN_RECORDS_FILE)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillRunRecord {
    pub run_id: String,
    pub skill_name: String,
    pub started_at_ms: i64,
    pub ended_at_ms: i64,
    pub origin: String,
    pub outcome: String,
    pub turn_count: usize,
    pub tool_action_count: usize,
    pub manual_fix_detected: bool,
    pub rollback_detected: bool,
    #[serde(default)]
    pub failure_types: Vec<String>,
    pub final_summary: String,
}

pub struct SkillRunBatch {
    pub records: Vec<SkillRunRecord>,
    #[allow(dead_code)]
    pub unread_record_count: usize,
    #[allow(dead_code)]
    pub next_offset: u64,
}

pub async fn append_skill_run_records(records: &[SkillRunRecord]) -> Result<usize> {
    if records.is_empty() {
        return Ok(0);
    }
    let _guard = skill_run_records_io_lock().lock().await;
    let path = skill_run_records_file_path().await;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await.map_err(|err| {
            miette!("failed to create skill run record dir: {err}")
        })?;
    }

    let mut existing_ids = HashSet::new();
    if let Ok(file) = OpenOptions::new().read(true).open(&path).await {
        let mut lines = BufReader::new(file).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let trimmed = line.trim().to_string();
            if trimmed.is_empty() {
                continue;
            }
            if let Ok(record) = serde_json::from_str::<SkillRunRecord>(&trimmed) {
                existing_ids.insert(record.run_id);
            }
        }
    }

    let mut batch = Vec::new();
    let mut appended = 0usize;
    for record in records {
        if !existing_ids.insert(record.run_id.clone()) {
            continue;
        }
        let mut bytes = serde_json::to_vec(record)
            .map_err(|err| miette!("failed to serialize skill run record: {err}"))?;
        bytes.push(b'\n');
        batch.extend(bytes);
        appended += 1;
    }

    if !batch.is_empty() {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await
            .map_err(|err| miette!("failed to open skill run records file: {err}"))?;
        file.write_all(&batch)
            .await
            .map_err(|err| miette!("failed to write skill run records: {err}"))?;
    }
    Ok(appended)
}

pub async fn load_skill_run_batch(max_records: usize, offset: u64) -> Result<SkillRunBatch> {
    let path = skill_run_records_file_path().await;
    let Ok(file) = OpenOptions::new().read(true).open(&path).await else {
        return Ok(SkillRunBatch {
            records: Vec::new(),
            unread_record_count: 0,
            next_offset: offset,
        });
    };
    let mut lines = BufReader::new(file).lines();
    let mut line_index = 0u64;
    let mut records = Vec::new();
    let mut unread_record_count = 0usize;
    while let Ok(Some(line)) = lines.next_line().await {
        let trimmed = line.trim().to_string();
        if trimmed.is_empty() {
            continue;
        }
        if line_index < offset {
            line_index += 1;
            continue;
        }
        if let Ok(record) = serde_json::from_str::<SkillRunRecord>(&trimmed) {
            if records.len() < max_records {
                records.push(record);
            } else {
                unread_record_count += 1;
            }
        }
        line_index += 1;
    }
    Ok(SkillRunBatch {
        records,
        unread_record_count,
        next_offset: line_index,
    })
}

pub async fn skill_run_record_count() -> Result<usize> {
    let path = skill_run_records_file_path().await;
    let Ok(file) = OpenOptions::new().read(true).open(&path).await else {
        return Ok(0);
    };
    let mut lines = BufReader::new(file).lines();
    let mut count = 0usize;
    while let Ok(Some(line)) = lines.next_line().await {
        if !line.trim().is_empty() {
            count += 1;
        }
    }
    Ok(count)
}
