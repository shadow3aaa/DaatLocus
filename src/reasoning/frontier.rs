use std::path::PathBuf;

use miette::{IntoDiagnostic, Result};
use serde::{Deserialize, Serialize};
use tokio::fs;

use crate::{
    daat_locus_paths::daat_locus_paths,
    persistence::{PersistenceFileMode, write_bytes_atomic},
};

use super::evaluation_artifacts::{
    EvaluationArtifactWorkflowCandidateEvaluation, EvaluationArtifactWorkflowMerge,
    EvaluationArtifactWorkflowPatch,
};

const FRONTIERS_DIR_NAME: &str = "sleep_frontiers";
const WORKFLOW_FRONTIER_FILE_NAME: &str = "workflow_frontier.json";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct WorkflowFrontierEntry {
    pub key: String,
    #[serde(default)]
    pub parent_keys: Vec<String>,
    #[serde(default)]
    pub generation: usize,
    pub group_key: String,
    pub candidate_kind: String,
    #[serde(default)]
    pub patch: Option<EvaluationArtifactWorkflowPatch>,
    #[serde(default)]
    pub merge: Option<EvaluationArtifactWorkflowMerge>,
    pub evaluation: EvaluationArtifactWorkflowCandidateEvaluation,
    #[serde(default)]
    pub applied_count: usize,
    #[serde(default)]
    pub last_selected_at_ms: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FrontierLineageStats {
    pub total_entries: usize,
    pub root_entries: usize,
    pub branched_entries: usize,
    pub max_generation: usize,
    pub total_parent_links: usize,
}

pub async fn load_workflow_frontier() -> Result<Vec<WorkflowFrontierEntry>> {
    load_json_array(workflow_frontier_file_path().await).await
}

pub async fn save_workflow_frontier(entries: &[WorkflowFrontierEntry]) -> Result<()> {
    save_json_array(workflow_frontier_file_path().await, entries).await
}

pub fn workflow_patch_frontier_entry_from_candidate(
    patch: &EvaluationArtifactWorkflowPatch,
    evaluation: &EvaluationArtifactWorkflowCandidateEvaluation,
) -> WorkflowFrontierEntry {
    WorkflowFrontierEntry {
        key: workflow_patch_key(patch),
        parent_keys: Vec::new(),
        generation: 0,
        group_key: format!("patch:{}", patch.workflow_id),
        candidate_kind: "patch".to_string(),
        patch: Some(patch.clone()),
        merge: None,
        evaluation: evaluation.clone(),
        applied_count: 0,
        last_selected_at_ms: None,
    }
}

pub fn workflow_merge_frontier_entry_from_candidate(
    merge: &EvaluationArtifactWorkflowMerge,
    evaluation: &EvaluationArtifactWorkflowCandidateEvaluation,
) -> WorkflowFrontierEntry {
    WorkflowFrontierEntry {
        key: workflow_merge_key(merge),
        parent_keys: Vec::new(),
        generation: 0,
        group_key: format!("merge:{}", merge.target_workflow_id),
        candidate_kind: "merge".to_string(),
        patch: None,
        merge: Some(merge.clone()),
        evaluation: evaluation.clone(),
        applied_count: 0,
        last_selected_at_ms: None,
    }
}

pub fn retain_workflow_frontier(
    existing: &[WorkflowFrontierEntry],
    incoming: &[WorkflowFrontierEntry],
    max_entries_per_group: usize,
) -> Vec<WorkflowFrontierEntry> {
    let combined = dedupe_workflow_frontier_entries(existing, incoming);
    let mut retained = Vec::new();

    let group_keys = combined
        .iter()
        .map(|entry| entry.group_key.clone())
        .collect::<std::collections::BTreeSet<_>>();
    for group_key in group_keys.iter() {
        let group_entries = combined
            .iter()
            .filter(|entry| &entry.group_key == group_key)
            .cloned()
            .collect::<Vec<_>>();
        let mut nondominated = nondominated_workflow_entries(&group_entries);
        nondominated.sort_by(|left, right| compare_workflow_entries(right, left));
        nondominated.truncate(max_entries_per_group);
        retained.extend(nondominated);
    }

    retained
}

pub fn select_workflow_patch_frontier_entries(
    entries: &[WorkflowFrontierEntry],
) -> Vec<WorkflowFrontierEntry> {
    let mut selected = Vec::new();
    let groups = entries
        .iter()
        .filter(|entry| entry.candidate_kind == "patch")
        .map(|entry| entry.group_key.clone())
        .collect::<std::collections::BTreeSet<_>>();
    for group in groups {
        if let Some(best) = entries
            .iter()
            .filter(|entry| entry.group_key == group && entry.candidate_kind == "patch")
            .cloned()
            .max_by(compare_workflow_entries)
        {
            selected.push(best);
        }
    }
    selected
}

pub fn select_workflow_merge_frontier_entries(
    entries: &[WorkflowFrontierEntry],
) -> Vec<WorkflowFrontierEntry> {
    let mut ordered = entries
        .iter()
        .filter(|entry| entry.candidate_kind == "merge")
        .cloned()
        .collect::<Vec<_>>();
    ordered.sort_by(|left, right| compare_workflow_entries(right, left));

    let mut selected = Vec::new();
    let mut used_workflows = std::collections::HashSet::<String>::new();
    for entry in ordered {
        let Some(merge) = entry.merge.as_ref() else {
            continue;
        };
        if used_workflows.contains(&merge.target_workflow_id)
            || merge
                .source_workflow_ids
                .iter()
                .any(|source| used_workflows.contains(source))
        {
            continue;
        }
        used_workflows.insert(merge.target_workflow_id.clone());
        for source in &merge.source_workflow_ids {
            used_workflows.insert(source.clone());
        }
        selected.push(entry);
    }
    selected
}

pub fn mark_workflow_frontier_selected(
    entries: &mut [WorkflowFrontierEntry],
    selected_keys: &[String],
) {
    let now = chrono::Utc::now().timestamp_millis();
    for entry in entries {
        if selected_keys.iter().any(|key| key == &entry.key) {
            entry.last_selected_at_ms = Some(now);
            entry.applied_count += 1;
        }
    }
}

pub fn workflow_frontier_lineage_stats(entries: &[WorkflowFrontierEntry]) -> FrontierLineageStats {
    frontier_lineage_stats(
        entries
            .iter()
            .map(|entry| (&entry.parent_keys, entry.generation)),
    )
}

async fn workflow_frontier_file_path() -> PathBuf {
    frontiers_dir().await.join(WORKFLOW_FRONTIER_FILE_NAME)
}

async fn frontiers_dir() -> PathBuf {
    let dir = daat_locus_paths()
        .await
        .state_dir()
        .join(FRONTIERS_DIR_NAME);
    let _ = fs::create_dir_all(&dir).await;
    dir
}

async fn load_json_array<T>(path: PathBuf) -> Result<Vec<T>>
where
    T: for<'de> Deserialize<'de>,
{
    let Ok(bytes) = fs::read(&path).await else {
        return Ok(Vec::new());
    };
    serde_json::from_slice(&bytes).into_diagnostic()
}

async fn save_json_array<T>(path: PathBuf, entries: &[T]) -> Result<()>
where
    T: Serialize,
{
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await.into_diagnostic()?;
    }
    let bytes = serde_json::to_vec_pretty(entries).into_diagnostic()?;
    write_bytes_atomic(path, bytes, PersistenceFileMode::Default)
        .await
        .into_diagnostic()
}

fn dedupe_workflow_frontier_entries(
    existing: &[WorkflowFrontierEntry],
    incoming: &[WorkflowFrontierEntry],
) -> Vec<WorkflowFrontierEntry> {
    let mut by_key = std::collections::BTreeMap::<String, WorkflowFrontierEntry>::new();
    for entry in existing.iter().chain(incoming.iter()) {
        by_key
            .entry(entry.key.clone())
            .and_modify(|current| {
                if compare_workflow_entries(entry, current).is_gt() {
                    *current = entry.clone();
                }
            })
            .or_insert_with(|| entry.clone());
    }
    by_key.into_values().collect()
}

fn nondominated_workflow_entries(entries: &[WorkflowFrontierEntry]) -> Vec<WorkflowFrontierEntry> {
    entries
        .iter()
        .filter(|entry| {
            !entries
                .iter()
                .any(|other| other.key != entry.key && workflow_entry_dominates(other, entry))
        })
        .cloned()
        .collect()
}

fn workflow_entry_dominates(left: &WorkflowFrontierEntry, right: &WorkflowFrontierEntry) -> bool {
    let left_accepted = usize::from(left.evaluation.accepted);
    let right_accepted = usize::from(right.evaluation.accepted);
    let left_score = left.evaluation.score;
    let right_score = right.evaluation.score;
    let left_size = workflow_entry_size_cost(left);
    let right_size = workflow_entry_size_cost(right);
    let left_applied = left.applied_count;
    let right_applied = right.applied_count;

    left_accepted >= right_accepted
        && left_score >= right_score
        && left_size <= right_size
        && left_applied <= right_applied
        && (left_accepted > right_accepted
            || left_score > right_score
            || left_size < right_size
            || left_applied < right_applied)
}

fn compare_workflow_entries(
    left: &WorkflowFrontierEntry,
    right: &WorkflowFrontierEntry,
) -> std::cmp::Ordering {
    usize::from(left.evaluation.accepted)
        .cmp(&usize::from(right.evaluation.accepted))
        .then_with(|| left.evaluation.score.total_cmp(&right.evaluation.score))
        .then_with(|| workflow_entry_size_cost(right).cmp(&workflow_entry_size_cost(left)))
        .then_with(|| right.applied_count.cmp(&left.applied_count))
}

fn workflow_entry_size_cost(entry: &WorkflowFrontierEntry) -> usize {
    match entry.candidate_kind.as_str() {
        "patch" => entry
            .patch
            .as_ref()
            .map(|patch| {
                patch.when_to_use_additions.len()
                    + patch.precondition_additions.len()
                    + patch.workflow_step_additions.len()
                    + patch.done_criteria_additions.len()
                    + patch.recovery_additions.len()
            })
            .unwrap_or(usize::MAX),
        "merge" => entry
            .merge
            .as_ref()
            .map(|merge| merge.source_workflow_ids.len())
            .unwrap_or(usize::MAX),
        _ => usize::MAX,
    }
}

fn workflow_patch_key(patch: &EvaluationArtifactWorkflowPatch) -> String {
    format!(
        "patch|{}|{}|{}|{}|{}|{}",
        patch.workflow_id,
        patch.when_to_use_additions.join("\n"),
        patch.precondition_additions.join("\n"),
        patch.workflow_step_additions.join("\n"),
        patch.done_criteria_additions.join("\n"),
        patch.recovery_additions.join("\n")
    )
}

fn workflow_merge_key(merge: &EvaluationArtifactWorkflowMerge) -> String {
    format!(
        "merge|{}|{}",
        merge.target_workflow_id,
        merge.source_workflow_ids.join("+")
    )
}

fn frontier_lineage_stats<'a>(
    entries: impl Iterator<Item = (&'a Vec<String>, usize)>,
) -> FrontierLineageStats {
    let mut stats = FrontierLineageStats::default();
    for (parent_keys, generation) in entries {
        stats.total_entries += 1;
        if parent_keys.is_empty() {
            stats.root_entries += 1;
        } else {
            stats.branched_entries += 1;
        }
        stats.total_parent_links += parent_keys.len();
        stats.max_generation = stats.max_generation.max(generation);
    }
    stats
}
