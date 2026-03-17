use std::{collections::BTreeMap, path::Path};

use miette::{Result, miette};
use serde::Deserialize;

use crate::reasoning::episode::EpisodeTask;

#[derive(Debug, Clone, Deserialize)]
pub struct SweTrainTaskRecord {
    pub instance_id: String,
    pub repo: String,
    pub base_commit: String,
    pub problem_statement: String,
    #[serde(default)]
    pub hints_text: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub test_patch: Option<String>,
    #[serde(default)]
    pub patch: Option<String>,
    #[serde(default)]
    pub fail_to_pass: Option<Vec<String>>,
    #[serde(default)]
    pub pass_to_pass: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
pub struct SweTrainSource {
    tasks: Vec<SweTrainTaskRecord>,
}

impl SweTrainSource {
    pub async fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let raw = tokio::fs::read_to_string(path)
            .await
            .map_err(|err| miette!("failed to read SWE-style training source {}: {err}", path.display()))?;
        Self::from_raw(path, &raw)
    }

    pub fn load_blocking(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let raw = std::fs::read_to_string(path)
            .map_err(|err| miette!("failed to read SWE-style training source {}: {err}", path.display()))?;
        Self::from_raw(path, &raw)
    }

    fn from_raw(path: &Path, raw: &str) -> Result<Self> {
        let tasks = if path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("jsonl"))
        {
            parse_jsonl(raw)?
        } else {
            parse_json(raw)?
        };

        Ok(Self { tasks })
    }

    pub fn tasks(&self) -> &[SweTrainTaskRecord] {
        &self.tasks
    }

    pub fn into_episode_tasks(self, max_steps: usize) -> Vec<EpisodeTask> {
        self.tasks
            .into_iter()
            .map(|task| episode_task_from_record(task, max_steps))
            .collect()
    }
}

fn parse_json(raw: &str) -> Result<Vec<SweTrainTaskRecord>> {
    serde_json::from_str(raw)
        .map_err(|err| miette!("failed to parse SWE-style task json: {err}"))
}

fn parse_jsonl(raw: &str) -> Result<Vec<SweTrainTaskRecord>> {
    let mut tasks = Vec::new();
    for (index, line) in raw.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let task = serde_json::from_str::<SweTrainTaskRecord>(line).map_err(|err| {
            miette!("failed to parse SWE-style task jsonl line {}: {err}", index + 1)
        })?;
        tasks.push(task);
    }
    Ok(tasks)
}

pub fn episode_task_from_record(record: SweTrainTaskRecord, max_steps: usize) -> EpisodeTask {
    let mut metadata = BTreeMap::new();
    metadata.insert("repo".to_string(), record.repo.clone());
    metadata.insert("base_commit".to_string(), record.base_commit.clone());
    if let Some(version) = &record.version {
        metadata.insert("version".to_string(), version.clone());
    }
    if let Some(hints) = &record.hints_text {
        metadata.insert("hints_text".to_string(), hints.clone());
    }
    if let Some(test_patch) = &record.test_patch {
        metadata.insert("test_patch".to_string(), test_patch.clone());
    }
    if let Some(patch) = &record.patch {
        metadata.insert("reference_patch".to_string(), patch.clone());
    }

    let mut success_criteria = Vec::new();
    if let Some(fail_to_pass) = &record.fail_to_pass {
        success_criteria.extend(
            fail_to_pass
                .iter()
                .map(|item| format!("make previously failing test pass: {item}")),
        );
    }
    if let Some(pass_to_pass) = &record.pass_to_pass {
        success_criteria.extend(
            pass_to_pass
                .iter()
                .map(|item| format!("preserve passing test: {item}")),
        );
    }
    if success_criteria.is_empty() {
        success_criteria.push("produce a repository state that satisfies the task statement".to_string());
    }
    let validation_commands = derive_validation_commands(
        record.fail_to_pass.as_deref(),
        record.pass_to_pass.as_deref(),
    );

    let instruction = match &record.hints_text {
        Some(hints) if !hints.trim().is_empty() => {
            format!("{}\n\nHints:\n{}", record.problem_statement.trim(), hints.trim())
        }
        _ => record.problem_statement.trim().to_string(),
    };

    EpisodeTask {
        id: record.instance_id.clone(),
        source: "swe_train_source".to_string(),
        title: format!("{} @ {}", record.repo, record.instance_id),
        instruction,
        workspace_hint: Some(record.repo),
        setup_commands: Vec::new(),
        validation_commands,
        success_criteria,
        max_steps,
        tags: vec!["swe".to_string(), "code".to_string(), "terminal".to_string()],
        metadata,
    }
}

fn derive_validation_commands(
    fail_to_pass: Option<&[String]>,
    pass_to_pass: Option<&[String]>,
) -> Vec<String> {
    let mut test_targets = Vec::new();
    if let Some(items) = fail_to_pass {
        test_targets.extend(items.iter().cloned());
    }
    if let Some(items) = pass_to_pass {
        test_targets.extend(items.iter().cloned());
    }
    test_targets.sort();
    test_targets.dedup();

    if test_targets.is_empty() {
        return Vec::new();
    }

    vec![format!("python -m pytest {}", test_targets.join(" "))]
}
