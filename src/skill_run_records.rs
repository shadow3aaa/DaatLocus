use std::{
    collections::HashSet,
    sync::{Mutex, OnceLock},
};

use miette::{Result, miette};
use serde::{Deserialize, Serialize};
use tokio::{
    fs::{self, OpenOptions},
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
};

use crate::daat_locus_paths::daat_locus_paths;

const SKILL_RUN_RECORDS_DIR: &str = "skills";
const SKILL_RUN_RECORDS_FILE: &str = "run_records.jsonl";

static SKILL_RUN_RECORDS_IO_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
static SKILL_RUN_RECORD_IDS: OnceLock<Mutex<Option<HashSet<String>>>> = OnceLock::new();

fn skill_run_records_io_lock() -> &'static tokio::sync::Mutex<()> {
    SKILL_RUN_RECORDS_IO_LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

fn skill_run_record_ids() -> &'static Mutex<Option<HashSet<String>>> {
    SKILL_RUN_RECORD_IDS.get_or_init(|| Mutex::new(None))
}

async fn skill_run_records_file_path() -> std::path::PathBuf {
    daat_locus_paths()
        .await
        .runtime_dir()
        .join(SKILL_RUN_RECORDS_DIR)
        .join(SKILL_RUN_RECORDS_FILE)
}

fn run_id_from_record_line(line: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(line)
        .ok()
        .and_then(|value| value.get("run_id")?.as_str().map(str::to_string))
}

async fn ensure_skill_run_record_ids(path: &std::path::Path) -> Result<()> {
    let loaded = skill_run_record_ids()
        .lock()
        .map_err(|err| miette!("skill run record id cache lock poisoned: {err}"))?
        .is_some();
    if loaded {
        return Ok(());
    }
    let mut ids = HashSet::new();
    if let Ok(file) = OpenOptions::new().read(true).open(path).await {
        let mut lines = BufReader::new(file).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Some(run_id) = run_id_from_record_line(trimmed) {
                ids.insert(run_id);
            }
        }
    }
    *skill_run_record_ids()
        .lock()
        .map_err(|err| miette!("skill run record id cache lock poisoned: {err}"))? = Some(ids);
    Ok(())
}

fn skill_run_record_id_is_new(run_id: &str) -> Result<bool> {
    let guard = skill_run_record_ids()
        .lock()
        .map_err(|err| miette!("skill run record id cache lock poisoned: {err}"))?;
    let ids = guard
        .as_ref()
        .ok_or_else(|| miette!("skill run record id cache was not loaded"))?;
    Ok(!ids.contains(run_id))
}

fn remember_skill_run_record_ids(run_ids: &[String]) -> Result<()> {
    let mut guard = skill_run_record_ids()
        .lock()
        .map_err(|err| miette!("skill run record id cache lock poisoned: {err}"))?;
    let ids = guard
        .as_mut()
        .ok_or_else(|| miette!("skill run record id cache was not loaded"))?;
    ids.extend(run_ids.iter().cloned());
    Ok(())
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

pub type SkillRunBatch = Vec<SkillRunRecord>;

pub async fn append_skill_run_records(records: &[SkillRunRecord]) -> Result<usize> {
    if records.is_empty() {
        return Ok(0);
    }
    let _guard = skill_run_records_io_lock().lock().await;
    let path = skill_run_records_file_path().await;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .await
            .map_err(|err| miette!("failed to create skill run record dir: {err}"))?;
    }

    ensure_skill_run_record_ids(&path).await?;

    let mut batch = Vec::new();
    let mut appended_ids = Vec::new();
    for record in records {
        if !skill_run_record_id_is_new(&record.run_id)? {
            continue;
        }
        let mut bytes = serde_json::to_vec(record)
            .map_err(|err| miette!("failed to serialize skill run record: {err}"))?;
        bytes.push(b'\n');
        batch.extend(bytes);
        appended_ids.push(record.run_id.clone());
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
        remember_skill_run_record_ids(&appended_ids)?;
    }
    Ok(appended_ids.len())
}

pub async fn load_skill_run_batch(max_records: usize, offset: u64) -> Result<SkillRunBatch> {
    let path = skill_run_records_file_path().await;
    let Ok(file) = OpenOptions::new().read(true).open(&path).await else {
        return Ok(Vec::new());
    };
    let mut lines = BufReader::new(file).lines();
    let mut line_index = 0u64;
    let mut records = Vec::new();
    while records.len() < max_records {
        let Ok(Some(line)) = lines.next_line().await else {
            break;
        };
        let trimmed = line.trim().to_string();
        if trimmed.is_empty() {
            continue;
        }
        if line_index < offset {
            line_index += 1;
            continue;
        }
        line_index += 1;
        if let Ok(record) = serde_json::from_str::<SkillRunRecord>(&trimmed) {
            records.push(record);
        }
    }
    Ok(records)
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
