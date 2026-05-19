use std::{
    borrow::Cow,
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
    sync::OnceLock,
};

use miette::{Result, miette};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::{
    fs,
    fs::OpenOptions,
    io::{AsyncBufReadExt, BufReader},
};

use crate::{
    daat_locus_paths::daat_locus_paths,
    persistence::{PersistenceFileMode, append_bytes_durable, write_bytes_atomic},
    workspace_app::paths::resolve_runtime_workspace_dir,
};

const MAX_SUMMARY_ITEMS: usize = 12;
const MAX_COMPACT_SUMMARY_CHARS: usize = 180;
const WORKFLOWS_DIR_NAME: &str = "workflows";
const WORKFLOW_RUN_RECORDS_FILE_NAME: &str = "run_records.jsonl";
static WORKFLOW_RUN_RECORDS_IO_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

mod builtin_workflow_bindings {
    include!(concat!(env!("OUT_DIR"), "/builtin_workflows.rs"));
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowOrigin {
    Builtin,
    Workspace,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WorkflowSpec {
    pub id: String,
    #[serde(default)]
    pub when_to_use: Vec<String>,
    #[serde(default)]
    pub preconditions: Vec<String>,
    #[serde(default)]
    pub workflow_steps: Vec<String>,
    #[serde(default)]
    pub done_criteria: Vec<String>,
    #[serde(default)]
    pub recovery: Vec<String>,
}

impl WorkflowSpec {
    fn normalize(mut self) -> Result<Self> {
        self.id = normalize_identifier(&self.id);
        self.when_to_use = normalize_string_list(self.when_to_use);
        self.preconditions = normalize_string_list(self.preconditions);
        self.workflow_steps = normalize_string_list(self.workflow_steps);
        self.done_criteria = normalize_string_list(self.done_criteria);
        self.recovery = normalize_string_list(self.recovery);

        if self.id.is_empty() {
            return Err(miette!("workflow.id cannot be empty"));
        }
        Ok(self)
    }

    pub fn primitive_summary(&self) -> WorkflowPrimitiveSummary {
        WorkflowPrimitiveSummary {
            id: self.id.clone(),
            origin: WorkflowOrigin::Workspace,
            capability_summary: compact_list_summary(&self.workflow_steps, 2),
            inputs_summary: compact_list_summary(&self.preconditions, 2),
            outputs_summary: compact_list_summary(&self.done_criteria, 2),
            when_to_use_summary: compact_list_summary(&self.when_to_use, 1),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WorkflowPrimitiveSummary {
    pub id: String,
    pub origin: WorkflowOrigin,
    #[serde(default)]
    pub capability_summary: String,
    #[serde(default)]
    pub inputs_summary: String,
    #[serde(default)]
    pub outputs_summary: String,
    #[serde(default)]
    pub when_to_use_summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WorkflowPrimitiveRoutingCatalog {
    pub primitive_ids: Vec<WorkflowPrimitiveId>,
    pub relevant_primitives: Vec<WorkflowPrimitiveSummary>,
    pub total_count: usize,
    pub relevant_omitted_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WorkflowPrimitiveId {
    pub id: String,
    pub origin: WorkflowOrigin,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct NewWorkflowSpec {
    pub id: String,
    #[serde(default)]
    pub when_to_use: Vec<String>,
    #[serde(default)]
    pub preconditions: Vec<String>,
    #[serde(default)]
    pub workflow_steps: Vec<String>,
    #[serde(default)]
    pub done_criteria: Vec<String>,
    #[serde(default)]
    pub recovery: Vec<String>,
}

impl NewWorkflowSpec {
    pub fn into_workflow_spec(self) -> WorkflowSpec {
        WorkflowSpec {
            id: self.id,
            when_to_use: self.when_to_use,
            preconditions: self.preconditions,
            workflow_steps: self.workflow_steps,
            done_criteria: self.done_criteria,
            recovery: self.recovery,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WorkflowRunRecord {
    pub run_id: String,
    pub workflow_id: String,
    pub started_at_ms: i64,
    pub ended_at_ms: i64,
    pub origin: String,
    pub outcome: WorkflowRunOutcome,
    pub turn_count: usize,
    pub tool_action_count: usize,
    pub manual_fix_detected: bool,
    pub rollback_detected: bool,
    #[serde(default)]
    pub failure_types: Vec<String>,
    pub final_summary: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowRunOutcome {
    Completed,
    Blocked,
    Abandoned,
    Superseded,
    NoProgress,
}

pub struct WorkflowRunBatch {
    pub records: Vec<WorkflowRunRecord>,
    pub unread_record_count: usize,
    pub next_offset: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WorkflowPatch {
    pub workflow_id: String,
    #[serde(default)]
    pub when_to_use_additions: Vec<String>,
    #[serde(default)]
    pub precondition_additions: Vec<String>,
    #[serde(default)]
    pub workflow_step_additions: Vec<String>,
    #[serde(default)]
    pub done_criteria_additions: Vec<String>,
    #[serde(default)]
    pub recovery_additions: Vec<String>,
}

struct StoredWorkflow {
    spec: WorkflowSpec,
    path: Option<PathBuf>,
    origin: WorkflowOrigin,
}

pub struct WorkflowStore {
    workflow_dir: PathBuf,
    workflows: BTreeMap<String, StoredWorkflow>,
}

impl WorkflowStore {
    pub async fn new() -> Self {
        let workflow_dir = resolve_runtime_workspace_dir()
            .unwrap()
            .join(WORKFLOWS_DIR_NAME);
        Self::open_scoped(workflow_dir).await
    }

    pub(crate) async fn open_scoped(workflow_dir: PathBuf) -> Self {
        let mut store = Self {
            workflow_dir,
            workflows: load_builtin_workflows(),
        };
        store.load_from_disk().await;
        store
    }

    pub fn get(&self, workflow_id: &str) -> Option<&WorkflowSpec> {
        self.workflows.get(workflow_id).map(|stored| &stored.spec)
    }

    pub fn workflow_origin(&self, workflow_id: &str) -> Option<WorkflowOrigin> {
        self.workflows.get(workflow_id).map(|stored| stored.origin)
    }

    pub fn workflow_path(&self, workflow_id: &str) -> Option<&Path> {
        self.workflows
            .get(workflow_id)
            .and_then(|stored| stored.path.as_deref())
    }

    pub fn workspace_list(&self) -> Vec<WorkflowSpec> {
        self.workflows
            .values()
            .filter(|stored| stored.origin == WorkflowOrigin::Workspace)
            .map(|stored| stored.spec.clone())
            .collect()
    }

    pub fn primitive_routing_catalog(
        &self,
        query: &str,
        limit: usize,
    ) -> WorkflowPrimitiveRoutingCatalog {
        let query_terms = workflow_relevance_terms(query);
        let mut primitive_ids = self
            .workflows
            .values()
            .map(|stored| WorkflowPrimitiveId {
                id: stored.spec.id.clone(),
                origin: stored.origin,
            })
            .collect::<Vec<_>>();
        primitive_ids.sort_by(|left, right| {
            workflow_origin_sort_key(left.origin)
                .cmp(&workflow_origin_sort_key(right.origin))
                .then_with(|| left.id.cmp(&right.id))
        });

        let mut scored = self
            .workflows
            .values()
            .filter_map(|stored| {
                if query_terms.is_empty() {
                    return None;
                }
                let score = workflow_route_score(&stored.spec, &query_terms);
                if score == 0 {
                    return None;
                }
                let mut summary = stored.spec.primitive_summary();
                summary.origin = stored.origin;
                Some((score, summary))
            })
            .collect::<Vec<_>>();
        scored.sort_by(|(left_score, left), (right_score, right)| {
            right_score
                .cmp(left_score)
                .then_with(|| {
                    workflow_origin_sort_key(left.origin)
                        .cmp(&workflow_origin_sort_key(right.origin))
                })
                .then_with(|| left.id.cmp(&right.id))
        });

        let total_count = self.workflows.len();
        let take_limit = limit.min(MAX_SUMMARY_ITEMS);
        let scored_count = scored.len();
        let relevant_primitives = scored
            .into_iter()
            .take(take_limit)
            .map(|(_, summary)| summary)
            .collect::<Vec<_>>();
        let relevant_omitted_count = scored_count.saturating_sub(relevant_primitives.len());
        WorkflowPrimitiveRoutingCatalog {
            primitive_ids,
            relevant_primitives,
            total_count,
            relevant_omitted_count,
        }
    }

    pub async fn create_workflow(&mut self, draft: NewWorkflowSpec) -> Result<WorkflowSpec> {
        if draft.id.trim().is_empty() {
            return Err(miette!("create_workflow requires non-empty id"));
        }
        if draft.when_to_use.is_empty() {
            return Err(miette!(
                "create_workflow requires at least one when_to_use item"
            ));
        }
        if draft.done_criteria.is_empty() {
            return Err(miette!(
                "create_workflow requires at least one done_criteria item"
            ));
        }

        let spec = draft.into_workflow_spec().normalize()?;
        if self.workflows.contains_key(&spec.id) {
            return Err(miette!("workflow_id `{}` already exists", spec.id));
        }
        let path = self.workflow_dir.join(format!("{}.md", spec.id));
        write_workflow_file(&path, &spec).await?;
        self.workflows.insert(
            spec.id.clone(),
            StoredWorkflow {
                spec: spec.clone(),
                path: Some(path),
                origin: WorkflowOrigin::Workspace,
            },
        );
        Ok(spec)
    }

    pub async fn apply_patch(&mut self, patch: WorkflowPatch) -> Result<WorkflowSpec> {
        let stored = self
            .workflows
            .get_mut(&patch.workflow_id)
            .ok_or_else(|| miette!("unknown workflow_id `{}`", patch.workflow_id))?;
        if stored.origin != WorkflowOrigin::Workspace {
            return Err(miette!(
                "builtin workflow `{}` is read-only and cannot be patched",
                patch.workflow_id
            ));
        }
        let path = stored.path.clone().ok_or_else(|| {
            miette!(
                "workspace workflow `{}` is missing backing path",
                patch.workflow_id
            )
        })?;

        let before = stored.spec.clone();
        extend_unique(
            &mut stored.spec.when_to_use,
            normalize_string_list(patch.when_to_use_additions),
        );
        extend_unique(
            &mut stored.spec.preconditions,
            normalize_string_list(patch.precondition_additions),
        );
        extend_unique(
            &mut stored.spec.workflow_steps,
            normalize_string_list(patch.workflow_step_additions),
        );
        extend_unique(
            &mut stored.spec.done_criteria,
            normalize_string_list(patch.done_criteria_additions),
        );
        extend_unique(
            &mut stored.spec.recovery,
            normalize_string_list(patch.recovery_additions),
        );

        stored.spec = stored.spec.clone().normalize()?;
        if !workflow_content_equal(&before, &stored.spec) {
            write_workflow_file(&path, &stored.spec).await?;
        }

        Ok(stored.spec.clone())
    }

    pub async fn replace_workspace_workflow(
        &mut self,
        workflow_id: &str,
        replacement: WorkflowSpec,
    ) -> Result<WorkflowSpec> {
        let workflow_id = normalize_identifier(workflow_id);
        if workflow_id.is_empty() {
            return Err(miette!("replace_workspace_workflow requires non-empty id"));
        }

        let stored = self
            .workflows
            .get_mut(&workflow_id)
            .ok_or_else(|| miette!("unknown workflow_id `{workflow_id}`"))?;
        if stored.origin != WorkflowOrigin::Workspace {
            return Err(miette!(
                "builtin workflow `{workflow_id}` is read-only and cannot be updated"
            ));
        }
        let path = stored
            .path
            .clone()
            .ok_or_else(|| miette!("workspace workflow `{workflow_id}` is missing backing path"))?;

        let replacement = replacement.normalize()?;
        if replacement.id != workflow_id {
            return Err(miette!(
                "replacement workflow id `{}` does not match target workflow_id `{workflow_id}`",
                replacement.id
            ));
        }
        if replacement.when_to_use.is_empty() {
            return Err(miette!(
                "update_workflow requires at least one when_to_use item"
            ));
        }
        if replacement.workflow_steps.is_empty() {
            return Err(miette!(
                "update_workflow requires at least one workflow_steps item"
            ));
        }
        if replacement.done_criteria.is_empty() {
            return Err(miette!(
                "update_workflow requires at least one done_criteria item"
            ));
        }

        let before = stored.spec.clone();
        stored.spec = replacement;
        if !workflow_content_equal(&before, &stored.spec) {
            write_workflow_file(&path, &stored.spec).await?;
        }

        Ok(stored.spec.clone())
    }

    pub async fn merge_workflows(
        &mut self,
        target_workflow_id: &str,
        source_workflow_ids: &[String],
        _reason: Option<String>,
    ) -> Result<WorkflowSpec> {
        if !self.workflows.contains_key(target_workflow_id) {
            return Err(miette!("unknown target workflow_id `{target_workflow_id}`"));
        }
        if self.workflow_origin(target_workflow_id) != Some(WorkflowOrigin::Workspace) {
            return Err(miette!(
                "builtin workflow `{target_workflow_id}` is read-only and cannot be merged"
            ));
        }

        let source_ids = source_workflow_ids
            .iter()
            .map(|item| normalize_identifier(item))
            .filter(|item| !item.is_empty() && item != target_workflow_id)
            .collect::<Vec<_>>();
        if source_ids.is_empty() {
            return Err(miette!(
                "merge_workflows requires at least one source workflow"
            ));
        }

        let sources = source_ids
            .iter()
            .map(|source_id| {
                self.workflows
                    .get(source_id)
                    .filter(|stored| stored.origin == WorkflowOrigin::Workspace)
                    .map(|stored| stored.spec.clone())
                    .ok_or_else(|| miette!("unknown source workflow_id `{source_id}`"))
            })
            .collect::<Result<Vec<_>>>()?;

        let target = self
            .workflows
            .get_mut(target_workflow_id)
            .ok_or_else(|| miette!("unknown target workflow_id `{target_workflow_id}`"))?;
        let target_path = target.path.clone().ok_or_else(|| {
            miette!("workspace workflow `{target_workflow_id}` is missing backing path")
        })?;

        for source in &sources {
            extend_unique(
                &mut target.spec.when_to_use,
                normalize_string_list(source.when_to_use.clone()),
            );
            extend_unique(
                &mut target.spec.preconditions,
                normalize_string_list(source.preconditions.clone()),
            );
            extend_unique(
                &mut target.spec.workflow_steps,
                normalize_string_list(source.workflow_steps.clone()),
            );
            extend_unique(
                &mut target.spec.done_criteria,
                normalize_string_list(source.done_criteria.clone()),
            );
            extend_unique(
                &mut target.spec.recovery,
                normalize_string_list(source.recovery.clone()),
            );
        }

        target.spec = target.spec.clone().normalize()?;
        write_workflow_file(&target_path, &target.spec).await?;

        for source_id in &source_ids {
            if let Some(stored) = self.workflows.remove(source_id)
                && let Some(path) = stored.path
            {
                let _ = tokio::fs::remove_file(path).await;
            }
        }

        self.workflows
            .get(target_workflow_id)
            .map(|stored| stored.spec.clone())
            .ok_or_else(|| miette!("unknown target workflow_id `{target_workflow_id}`"))
    }

    pub async fn shutdown(self) {}

    async fn load_from_disk(&mut self) {
        let _ = tokio::fs::create_dir_all(&self.workflow_dir).await;
        let Ok(mut entries) = tokio::fs::read_dir(&self.workflow_dir).await else {
            return;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("md") {
                continue;
            }
            let Ok(content) = tokio::fs::read_to_string(&path).await else {
                continue;
            };
            match parse_workflow_file(&content) {
                Ok(spec) => {
                    if self.workflows.contains_key(&spec.id) {
                        tracing::warn!(
                            "workspace workflow id `{}` conflicts with existing builtin/workspace definition at {}; skipping",
                            spec.id,
                            path.display()
                        );
                        continue;
                    }
                    self.workflows.insert(
                        spec.id.clone(),
                        StoredWorkflow {
                            spec,
                            path: Some(path),
                            origin: WorkflowOrigin::Workspace,
                        },
                    );
                }
                Err(err) => {
                    tracing::warn!("failed to parse workflow file {}: {err:?}", path.display());
                }
            }
        }
    }
}

fn load_builtin_workflows() -> BTreeMap<String, StoredWorkflow> {
    let mut workflows = BTreeMap::new();
    for (source_name, content) in builtin_workflow_bindings::BUILTIN_WORKFLOW_SOURCES {
        match parse_workflow_file(content) {
            Ok(spec) => {
                if workflows.contains_key(&spec.id) {
                    tracing::warn!(
                        "duplicate builtin workflow id `{}` detected in source {}; keeping first definition",
                        spec.id,
                        source_name
                    );
                    continue;
                }
                workflows.insert(
                    spec.id.clone(),
                    StoredWorkflow {
                        spec,
                        path: None,
                        origin: WorkflowOrigin::Builtin,
                    },
                );
            }
            Err(err) => {
                tracing::warn!(
                    "failed to parse builtin workflow source {}: {err:?}",
                    source_name
                );
            }
        }
    }
    workflows
}

pub async fn load_workflow_run_batch() -> Result<WorkflowRunBatch> {
    let workflow_run_records_io_guard = workflow_run_records_io_lock().lock().await;
    let path = workflow_run_records_file_path().await;
    let bytes = match fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            drop(workflow_run_records_io_guard);
            return Ok(WorkflowRunBatch {
                records: Vec::new(),
                unread_record_count: 0,
                next_offset: 0,
            });
        }
        Err(err) => {
            drop(workflow_run_records_io_guard);
            return Err(miette!(
                "failed to read workflow run records {}: {err}",
                path.display()
            ));
        }
    };

    let mut offset = 0u64;
    let mut records = Vec::new();
    for chunk in bytes.split_inclusive(|byte| *byte == b'\n') {
        offset += chunk.len() as u64;
        let line = std::str::from_utf8(chunk)
            .map(str::trim)
            .unwrap_or_default();
        if line.is_empty() {
            continue;
        }
        let record: WorkflowRunRecord = serde_json::from_str(line).map_err(|err| {
            miette!(
                "failed to parse workflow run record from {}: {err}",
                path.display()
            )
        })?;
        records.push(record);
    }
    let unread_record_count = records.len();
    drop(workflow_run_records_io_guard);
    Ok(WorkflowRunBatch {
        records,
        unread_record_count,
        next_offset: offset,
    })
}

pub async fn workflow_run_record_count() -> Result<usize> {
    let workflow_run_records_io_guard = workflow_run_records_io_lock().lock().await;
    let path = workflow_run_records_file_path().await;
    let file = match OpenOptions::new().read(true).open(&path).await {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            drop(workflow_run_records_io_guard);
            return Ok(0);
        }
        Err(err) => {
            drop(workflow_run_records_io_guard);
            return Err(miette!(
                "failed to open workflow run records {}: {err}",
                path.display()
            ));
        }
    };
    let mut lines = BufReader::new(file).lines();
    let mut records = 0usize;
    while let Some(line) = lines.next_line().await.map_err(|err| {
        miette!(
            "failed to read workflow run records {}: {err}",
            path.display()
        )
    })? {
        if !line.trim().is_empty() {
            records += 1;
        }
    }
    drop(workflow_run_records_io_guard);
    Ok(records)
}

pub async fn append_workflow_run_records(records: &[WorkflowRunRecord]) -> Result<usize> {
    if records.is_empty() {
        return Ok(0);
    }

    let workflow_run_records_io_guard = workflow_run_records_io_lock().lock().await;
    let path = workflow_run_records_file_path().await;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await.map_err(|err| {
            miette!(
                "failed to create workflow run record directory {}: {err}",
                parent.display()
            )
        })?;
    }

    let mut existing_ids = HashSet::new();
    if let Ok(file) = OpenOptions::new().read(true).open(&path).await {
        let mut lines = BufReader::new(file).lines();
        while let Some(line) = lines.next_line().await.map_err(|err| {
            miette!(
                "failed to read workflow run records for dedupe {}: {err}",
                path.display()
            )
        })? {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let record: WorkflowRunRecord = serde_json::from_str(trimmed).map_err(|err| {
                miette!(
                    "failed to parse workflow run record during dedupe {}: {err}",
                    path.display()
                )
            })?;
            existing_ids.insert(record.run_id);
        }
    }

    let mut appended = 0usize;
    let mut batch = Vec::new();
    for record in records {
        if !existing_ids.insert(record.run_id.clone()) {
            continue;
        }
        let mut bytes = serde_json::to_vec(record)
            .map_err(|err| miette!("failed to serialize workflow run record: {err}"))?;
        bytes.push(b'\n');
        batch.extend(bytes);
        appended += 1;
    }
    if !batch.is_empty() {
        append_bytes_durable(path.clone(), batch)
            .await
            .map_err(|err| {
                miette!(
                    "failed to append workflow run record {}: {err}",
                    path.display()
                )
            })?;
    }
    drop(workflow_run_records_io_guard);
    Ok(appended)
}

pub async fn compact_workflow_run_record_file(consumed_offset: u64) -> Result<()> {
    let workflow_run_records_io_guard = workflow_run_records_io_lock().lock().await;
    let path = workflow_run_records_file_path().await;
    let bytes = match fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            drop(workflow_run_records_io_guard);
            return Ok(());
        }
        Err(err) => {
            drop(workflow_run_records_io_guard);
            return Err(miette!(
                "failed to read workflow run records {} for compaction: {err}",
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
        miette!(
            "failed to rewrite workflow run records {} during compaction: {err}",
            path.display()
        )
    })?;
    drop(workflow_run_records_io_guard);
    Ok(())
}

#[derive(Debug, Deserialize, Serialize)]
struct WorkflowFrontmatter {
    id: String,
}

fn parse_workflow_file(content: &str) -> Result<WorkflowSpec> {
    let content = normalize_workflow_line_endings(content);
    let (frontmatter_text, body) = split_frontmatter(content.as_ref())?;
    let frontmatter: WorkflowFrontmatter = serde_yaml::from_str(frontmatter_text)
        .map_err(|err| miette!("parse workflow frontmatter failed: {err}"))?;
    let sections = parse_markdown_sections(body);
    WorkflowSpec {
        id: frontmatter.id,
        when_to_use: parse_markdown_list(sections.get("When To Use")),
        preconditions: parse_markdown_list(sections.get("Preconditions")),
        workflow_steps: parse_markdown_list(sections.get("Workflow")),
        done_criteria: parse_markdown_list(sections.get("Done Criteria")),
        recovery: parse_markdown_list(sections.get("Recovery")),
    }
    .normalize()
}

fn normalize_workflow_line_endings(content: &str) -> Cow<'_, str> {
    if content.contains('\r') {
        Cow::Owned(content.replace("\r\n", "\n").replace('\r', "\n"))
    } else {
        Cow::Borrowed(content)
    }
}

async fn write_workflow_file(path: &Path, spec: &WorkflowSpec) -> Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|err| {
            miette!(
                "create workflow directory {} failed: {err}",
                parent.display()
            )
        })?;
    }

    let frontmatter = WorkflowFrontmatter {
        id: spec.id.clone(),
    };
    let frontmatter_text = serde_yaml::to_string(&frontmatter)
        .map_err(|err| miette!("serialize workflow frontmatter failed: {err}"))?;
    let body = render_workflow_markdown_body(spec);
    let content = format!("---\n{}---\n\n{}", frontmatter_text, body);
    write_bytes_atomic(
        path.to_path_buf(),
        content.into_bytes(),
        PersistenceFileMode::Default,
    )
    .await
    .map_err(|err| miette!("write workflow file {} failed: {err}", path.display()))
}

fn render_workflow_markdown_body(spec: &WorkflowSpec) -> String {
    [
        render_section("When To Use", &spec.when_to_use, false),
        render_section("Preconditions", &spec.preconditions, false),
        render_section("Workflow", &spec.workflow_steps, true),
        render_section("Done Criteria", &spec.done_criteria, false),
        render_section("Recovery", &spec.recovery, false),
    ]
    .join("\n\n")
}

fn render_section(title: &str, items: &[String], ordered: bool) -> String {
    let mut lines = vec![format!("## {title}")];
    if items.is_empty() {
        lines.push("- <empty>".to_string());
    } else if ordered {
        lines.extend(
            items
                .iter()
                .enumerate()
                .map(|(index, item)| format!("{}. {}", index + 1, item)),
        );
    } else {
        lines.extend(items.iter().map(|item| format!("- {item}")));
    }
    lines.join("\n")
}

fn split_frontmatter(content: &str) -> Result<(&str, &str)> {
    let rest = content
        .strip_prefix("---\n")
        .ok_or_else(|| miette!("workflow file missing frontmatter start"))?;
    let end = rest
        .find("\n---\n")
        .ok_or_else(|| miette!("workflow file missing frontmatter end"))?;
    Ok((&rest[..end], &rest[end + 5..]))
}

fn parse_markdown_sections(body: &str) -> BTreeMap<String, String> {
    let mut sections = BTreeMap::new();
    let mut current: Option<String> = None;
    let mut current_lines = Vec::new();

    for line in body.lines() {
        if let Some(title) = line.trim().strip_prefix("## ") {
            if let Some(current_title) = current.replace(title.trim().to_string()) {
                sections.insert(current_title, current_lines.join("\n"));
                current_lines.clear();
            }
            continue;
        }
        if current.is_some() {
            current_lines.push(line.to_string());
        }
    }

    if let Some(current_title) = current {
        sections.insert(current_title, current_lines.join("\n"));
    }
    sections
}

fn parse_markdown_list(section: Option<&String>) -> Vec<String> {
    let Some(section) = section else {
        return Vec::new();
    };
    section
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter_map(|line| {
            if let Some(item) = line.strip_prefix("- ") {
                return Some(item.trim().to_string());
            }
            if let Some((prefix, rest)) = line.split_once(". ")
                && prefix.chars().all(|ch| ch.is_ascii_digit())
            {
                return Some(rest.trim().to_string());
            }
            None
        })
        .filter(|item| item != "<empty>")
        .collect()
}

fn workflow_content_equal(left: &WorkflowSpec, right: &WorkflowSpec) -> bool {
    left.id == right.id
        && left.when_to_use == right.when_to_use
        && left.preconditions == right.preconditions
        && left.workflow_steps == right.workflow_steps
        && left.done_criteria == right.done_criteria
        && left.recovery == right.recovery
}

fn compact_list_summary(items: &[String], limit: usize) -> String {
    if items.is_empty() || limit == 0 {
        return String::new();
    }
    let mut parts = items
        .iter()
        .take(limit)
        .map(|item| compact_summary_text(item))
        .collect::<Vec<_>>();
    if items.len() > limit {
        parts.push(format!("+{} more", items.len() - limit));
    }
    parts.join("; ")
}

fn compact_summary_text(text: &str) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= MAX_COMPACT_SUMMARY_CHARS {
        return compact;
    }
    let head = compact
        .chars()
        .take(MAX_COMPACT_SUMMARY_CHARS)
        .collect::<String>();
    format!("{head}...")
}

fn workflow_route_score(spec: &WorkflowSpec, query_terms: &[String]) -> usize {
    if query_terms.is_empty() {
        return 0;
    }
    let candidate = workflow_relevance_text(spec);
    query_terms
        .iter()
        .filter(|term| candidate.contains(term.as_str()))
        .count()
}

fn workflow_relevance_text(spec: &WorkflowSpec) -> String {
    let mut parts = vec![spec.id.replace(['-', '_'], " ")];
    parts.extend(spec.when_to_use.iter().cloned());
    parts.extend(spec.preconditions.iter().cloned());
    parts.extend(spec.workflow_steps.iter().cloned());
    parts.extend(spec.done_criteria.iter().cloned());
    parts.extend(spec.recovery.iter().cloned());
    parts.join("\n").to_lowercase()
}

fn workflow_relevance_terms(query: &str) -> Vec<String> {
    let mut terms = HashSet::new();
    let mut ascii_run = String::new();
    let mut cjk_run = String::new();

    for ch in query.to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            flush_cjk_terms(&mut terms, &mut cjk_run);
            ascii_run.push(ch);
        } else if is_cjk_char(ch) {
            flush_ascii_term(&mut terms, &mut ascii_run);
            cjk_run.push(ch);
        } else {
            flush_ascii_term(&mut terms, &mut ascii_run);
            flush_cjk_terms(&mut terms, &mut cjk_run);
        }
    }
    flush_ascii_term(&mut terms, &mut ascii_run);
    flush_cjk_terms(&mut terms, &mut cjk_run);

    let mut terms = terms.into_iter().collect::<Vec<_>>();
    terms.sort();
    terms
}

fn flush_ascii_term(terms: &mut HashSet<String>, current: &mut String) {
    if current.chars().count() >= 2 && !is_stop_term(current) {
        terms.insert(current.clone());
    }
    current.clear();
}

fn flush_cjk_terms(terms: &mut HashSet<String>, current: &mut String) {
    let chars = current.chars().collect::<Vec<_>>();
    match chars.len() {
        0 => {}
        1 => {
            let term = chars[0].to_string();
            if !is_stop_term(&term) {
                terms.insert(term);
            }
        }
        _ => {
            let full = chars.iter().collect::<String>();
            if !is_stop_term(&full) {
                terms.insert(full);
            }
            for pair in chars.windows(2) {
                let term = pair.iter().collect::<String>();
                if !is_stop_term(&term) {
                    terms.insert(term);
                }
            }
        }
    }
    current.clear();
}

fn is_cjk_char(ch: char) -> bool {
    ('\u{4E00}'..='\u{9FFF}').contains(&ch)
        || ('\u{3400}'..='\u{4DBF}').contains(&ch)
        || ('\u{F900}'..='\u{FAFF}').contains(&ch)
}

fn is_stop_term(term: &str) -> bool {
    matches!(
        term,
        "a" | "an"
            | "and"
            | "are"
            | "as"
            | "for"
            | "in"
            | "is"
            | "it"
            | "of"
            | "on"
            | "or"
            | "the"
            | "to"
            | "user"
            | "with"
            | "好"
            | "的"
            | "了"
            | "就"
            | "按"
            | "此"
    )
}

fn workflow_origin_sort_key(origin: WorkflowOrigin) -> u8 {
    match origin {
        WorkflowOrigin::Builtin => 0,
        WorkflowOrigin::Workspace => 1,
    }
}

fn normalize_identifier(value: &str) -> String {
    value
        .trim()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

fn normalize_string_list(items: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut normalized = Vec::new();
    for item in items {
        let trimmed = item.trim();
        if trimmed.is_empty() {
            continue;
        }
        if seen.insert(trimmed.to_string()) {
            normalized.push(trimmed.to_string());
        }
    }
    normalized
}

fn extend_unique(target: &mut Vec<String>, additions: Vec<String>) {
    if additions.is_empty() {
        return;
    }
    let mut existing = target.iter().cloned().collect::<HashSet<_>>();
    for item in additions {
        if existing.insert(item.clone()) {
            target.push(item);
        }
    }
}

async fn workflow_run_records_file_path() -> PathBuf {
    daat_locus_paths()
        .await
        .state_dir()
        .join(WORKFLOWS_DIR_NAME)
        .join(WORKFLOW_RUN_RECORDS_FILE_NAME)
}

fn workflow_run_records_io_lock() -> &'static tokio::sync::Mutex<()> {
    WORKFLOW_RUN_RECORDS_IO_LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use tempfile::TempDir;

    #[tokio::test]
    async fn create_workflow_writes_markdown_and_can_reload() {
        let temp_dir = TempDir::new().expect("create workflow temp dir");
        let primary = temp_dir.path().join("workflows");
        let mut store = WorkflowStore::open_scoped(primary.clone()).await;
        let created = store
            .create_workflow(NewWorkflowSpec {
                id: "repair-flaky-test-pipeline".to_string(),
                when_to_use: vec!["flaky test failure".to_string()],
                preconditions: vec!["failing logs available".to_string()],
                workflow_steps: vec!["collect evidence".to_string(), "verify fix".to_string()],
                done_criteria: vec!["result is stable".to_string()],
                recovery: vec!["return to previous stable state".to_string()],
            })
            .await
            .expect("create workflow");

        assert_eq!(created.id, "repair-flaky-test-pipeline");
        assert!(primary.join("repair-flaky-test-pipeline.md").exists());

        let reloaded = WorkflowStore::open_scoped(primary.clone()).await;
        let loaded = reloaded
            .get("repair-flaky-test-pipeline")
            .expect("reloaded workflow");
        assert_eq!(loaded.workflow_steps.len(), 2);
    }

    #[tokio::test]
    async fn primitive_routing_catalog_includes_all_ids_and_ranks_relevant_primitives() {
        let temp_dir = TempDir::new().expect("create workflow temp dir");
        let primary = temp_dir.path().join("workflows");
        let mut store = WorkflowStore::open_scoped(primary).await;
        store
            .create_workflow(NewWorkflowSpec {
                id: "zephyr-quartz-inspection".to_string(),
                when_to_use: vec!["inspect zephyr quartz artifacts".to_string()],
                preconditions: vec!["artifact path is known".to_string()],
                workflow_steps: vec!["inspect artifact metadata".to_string()],
                done_criteria: vec!["artifact findings are summarized".to_string()],
                recovery: vec![],
            })
            .await
            .expect("create matching workflow");
        store
            .create_workflow(NewWorkflowSpec {
                id: "unrelated-ritual".to_string(),
                when_to_use: vec!["handle unrelated ritual tasks".to_string()],
                preconditions: vec![],
                workflow_steps: vec!["perform unrelated step".to_string()],
                done_criteria: vec!["unrelated result exists".to_string()],
                recovery: vec![],
            })
            .await
            .expect("create unrelated workflow");

        let catalog = store.primitive_routing_catalog("please inspect zephyr quartz", 8);
        let primitive_ids = catalog
            .primitive_ids
            .iter()
            .map(|summary| summary.id.as_str())
            .collect::<Vec<_>>();
        let relevant_ids = catalog
            .relevant_primitives
            .iter()
            .map(|summary| summary.id.as_str())
            .collect::<Vec<_>>();

        assert!(primitive_ids.contains(&"zephyr-quartz-inspection"));
        assert!(primitive_ids.contains(&"unrelated-ritual"));
        assert!(relevant_ids.contains(&"zephyr-quartz-inspection"));
        assert!(!relevant_ids.contains(&"unrelated-ritual"));
        assert_eq!(catalog.total_count, catalog.primitive_ids.len());
    }

    #[test]
    fn primitive_summary_exposes_primitive_io_contract() {
        let summary = WorkflowSpec {
            id: "modify-local-project".to_string(),
            when_to_use: vec!["local project needs edits".to_string()],
            preconditions: vec!["project path is known".to_string()],
            workflow_steps: vec![
                "inspect relevant code".to_string(),
                "apply targeted edits".to_string(),
                "avoid unrelated files".to_string(),
            ],
            done_criteria: vec!["requested change is implemented".to_string()],
            recovery: vec![],
        }
        .primitive_summary();

        assert_eq!(
            summary.capability_summary,
            "inspect relevant code; apply targeted edits; +1 more"
        );
        assert_eq!(summary.inputs_summary, "project path is known");
        assert_eq!(summary.outputs_summary, "requested change is implemented");
        assert_eq!(summary.when_to_use_summary, "local project needs edits");
    }

    #[tokio::test]
    async fn builtin_workflow_ids_cannot_be_overwritten() {
        let temp_dir = TempDir::new().expect("create workflow temp dir");
        let primary = temp_dir.path().join("workflows");
        let mut store = WorkflowStore::open_scoped(primary).await;

        let err = store
            .create_workflow(NewWorkflowSpec {
                id: "author-workspace-app".to_string(),
                when_to_use: vec!["test".to_string()],
                preconditions: vec![],
                workflow_steps: vec!["step".to_string()],
                done_criteria: vec!["done".to_string()],
                recovery: vec![],
            })
            .await
            .expect_err("builtin workflow id should be reserved");

        assert!(err.to_string().contains("already exists"));
    }

    #[tokio::test]
    async fn builtin_workflows_are_read_only() {
        let temp_dir = TempDir::new().expect("create workflow temp dir");
        let primary = temp_dir.path().join("workflows");
        let mut store = WorkflowStore::open_scoped(primary).await;

        let err = store
            .apply_patch(WorkflowPatch {
                workflow_id: "author-workspace-app".to_string(),
                when_to_use_additions: vec!["extra".to_string()],
                precondition_additions: Vec::new(),
                workflow_step_additions: Vec::new(),
                done_criteria_additions: Vec::new(),
                recovery_additions: Vec::new(),
            })
            .await
            .expect_err("builtin workflow patch should be rejected");

        assert!(err.to_string().contains("read-only"));
    }

    #[tokio::test]
    async fn replace_workspace_workflow_rewrites_complete_spec() {
        let temp_dir = TempDir::new().expect("create workflow temp dir");
        let primary = temp_dir.path().join("workflows");
        let mut store = WorkflowStore::open_scoped(primary.clone()).await;
        store
            .create_workflow(NewWorkflowSpec {
                id: "search-todays-news".to_string(),
                when_to_use: vec!["search current news".to_string()],
                preconditions: vec!["network is available".to_string()],
                workflow_steps: vec![
                    "search aggregator".to_string(),
                    "repeat fallback searches".to_string(),
                ],
                done_criteria: vec!["sent news summary".to_string()],
                recovery: vec!["keep searching".to_string()],
            })
            .await
            .expect("create workflow");

        let updated = store
            .replace_workspace_workflow(
                "search-todays-news",
                WorkflowSpec {
                    id: "search-todays-news".to_string(),
                    when_to_use: vec!["user asks for today's news".to_string()],
                    preconditions: vec!["date scope is known".to_string()],
                    workflow_steps: vec![
                        "choose approved news sources".to_string(),
                        "verify publication dates".to_string(),
                        "send concise summary".to_string(),
                    ],
                    done_criteria: vec!["summary cites sources".to_string()],
                    recovery: vec!["return a limited summary if sources are sparse".to_string()],
                },
            )
            .await
            .expect("replace workflow");

        assert_eq!(updated.workflow_steps.len(), 3);
        assert!(
            !updated
                .workflow_steps
                .iter()
                .any(|step| step == "repeat fallback searches")
        );

        let reloaded = WorkflowStore::open_scoped(primary).await;
        let loaded = reloaded
            .get("search-todays-news")
            .expect("reloaded updated workflow");
        assert_eq!(
            loaded.workflow_steps,
            vec![
                "choose approved news sources",
                "verify publication dates",
                "send concise summary"
            ]
        );
    }

    #[tokio::test]
    async fn replace_rejects_builtin_workflow() {
        let temp_dir = TempDir::new().expect("create workflow temp dir");
        let primary = temp_dir.path().join("workflows");
        let mut store = WorkflowStore::open_scoped(primary).await;

        let err = store
            .replace_workspace_workflow(
                "author-workspace-app",
                WorkflowSpec {
                    id: "author-workspace-app".to_string(),
                    when_to_use: vec!["test".to_string()],
                    preconditions: vec![],
                    workflow_steps: vec!["step".to_string()],
                    done_criteria: vec!["done".to_string()],
                    recovery: vec![],
                },
            )
            .await
            .expect_err("builtin workflow update should be rejected");

        assert!(err.to_string().contains("read-only"));
    }

    #[test]
    fn parse_workflow_file_accepts_crlf_line_endings() {
        let spec = parse_workflow_file(
            "---\r\nid: crlf-workflow\r\n---\r\n\r\n## When To Use\r\n- Windows checkout\r\n\r\n## Preconditions\r\n- A workflow file uses CRLF\r\n\r\n## Workflow\r\n1. Parse frontmatter\r\n\r\n## Done Criteria\r\n- Workflow is loaded\r\n\r\n## Recovery\r\n- Retry with normalized line endings\r\n",
        )
        .expect("parse CRLF workflow");

        assert_eq!(spec.id, "crlf-workflow");
        assert_eq!(spec.workflow_steps, vec!["Parse frontmatter"]);
        assert_eq!(spec.done_criteria, vec!["Workflow is loaded"]);
    }

    #[tokio::test]
    async fn merge_workflows_deletes_sources_and_updates_target() {
        let temp_dir = TempDir::new().expect("create workflow temp dir");
        let primary = temp_dir.path().join("workflows");
        let mut store = WorkflowStore::open_scoped(primary.clone()).await;
        let target = store
            .create_workflow(NewWorkflowSpec {
                id: "investigate-runtime-failure".to_string(),
                when_to_use: vec!["runtime failure".to_string()],
                preconditions: vec![],
                workflow_steps: vec!["collect logs".to_string()],
                done_criteria: vec!["cause is clear".to_string()],
                recovery: vec![],
            })
            .await
            .expect("create target");
        let source = store
            .create_workflow(NewWorkflowSpec {
                id: "investigate-runtime-errors".to_string(),
                when_to_use: vec!["runtime error".to_string()],
                preconditions: vec![],
                workflow_steps: vec!["locate root cause".to_string()],
                done_criteria: vec!["fix direction is clear".to_string()],
                recovery: vec!["rollback".to_string()],
            })
            .await
            .expect("create source");

        let merged = store
            .merge_workflows(
                &target.id,
                std::slice::from_ref(&source.id),
                Some("duplicate".to_string()),
            )
            .await
            .expect("merge workflows");

        assert!(
            merged
                .workflow_steps
                .iter()
                .any(|item| item == "locate root cause")
        );
        assert!(store.get(&source.id).is_none());
        assert!(!primary.join(format!("{}.md", source.id)).exists());
    }

    #[tokio::test]
    async fn workflow_run_records_are_appended_and_deduped() {
        let temp_dir = TempDir::new().expect("workflow run record temp dir");
        let previous_home = env::var("DAAT_LOCUS_HOME").ok();
        unsafe {
            env::set_var("DAAT_LOCUS_HOME", temp_dir.path());
        }

        let record = WorkflowRunRecord {
            run_id: "workflow-run:run-1".to_string(),
            workflow_id: "repair-flaky-test-pipeline".to_string(),
            started_at_ms: 1,
            ended_at_ms: 2,
            origin: "event:test".to_string(),
            outcome: WorkflowRunOutcome::Completed,
            turn_count: 2,
            tool_action_count: 3,
            manual_fix_detected: false,
            rollback_detected: false,
            failure_types: vec!["tool_failure".to_string()],
            final_summary: "completed".to_string(),
        };
        let mut later_record = record.clone();
        later_record.run_id = "workflow-run:run-2".to_string();
        later_record.origin = "event:later".to_string();

        append_workflow_run_records(&[record.clone(), record.clone()])
            .await
            .expect("append workflow run records");
        let batch = load_workflow_run_batch()
            .await
            .expect("load workflow run records");
        let count = workflow_run_record_count()
            .await
            .expect("count workflow run records");
        append_workflow_run_records(&[later_record.clone()])
            .await
            .expect("append later workflow run record");
        compact_workflow_run_record_file(batch.next_offset)
            .await
            .expect("compact consumed workflow run records");
        let remaining_batch = load_workflow_run_batch()
            .await
            .expect("load remaining workflow run records");
        let remaining_count = workflow_run_record_count()
            .await
            .expect("count remaining workflow run records");

        match previous_home {
            Some(previous_home) => unsafe {
                env::set_var("DAAT_LOCUS_HOME", previous_home);
            },
            None => unsafe {
                env::remove_var("DAAT_LOCUS_HOME");
            },
        }

        assert_eq!(batch.records.len(), 1);
        assert_eq!(batch.unread_record_count, 1);
        assert!(batch.next_offset > 0);
        assert_eq!(count, 1);
        assert_eq!(batch.records[0].run_id, record.run_id);
        assert_eq!(batch.records[0].workflow_id, record.workflow_id);
        assert_eq!(remaining_count, 1);
        assert_eq!(remaining_batch.records.len(), 1);
        assert_eq!(remaining_batch.records[0].run_id, later_record.run_id);
    }
}
