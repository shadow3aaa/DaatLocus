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
const PRIMITIVE_RUN_RECORDS_FILE_NAME: &str = "run_records.jsonl";
static PRIMITIVE_RUN_RECORDS_IO_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

mod builtin_primitive_bindings {
    include!(concat!(env!("OUT_DIR"), "/builtin_workflows.rs"));
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PrimitiveOrigin {
    Builtin,
    Workspace,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PrimitiveSpec {
    pub id: String,
    #[serde(default)]
    pub when_to_use: Vec<String>,
    #[serde(default)]
    pub preconditions: Vec<String>,
    #[serde(default)]
    pub primitive_steps: Vec<String>,
    #[serde(default)]
    pub done_criteria: Vec<String>,
    #[serde(default)]
    pub recovery: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PrimitiveComposition {
    pub composition_id: String,
    pub primitive_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PrimitiveActivation {
    Single { primitive: PrimitiveSpec },
    Composition { composition: PrimitiveComposition },
}

impl PrimitiveSpec {
    fn normalize(mut self) -> Result<Self> {
        self.id = normalize_identifier(&self.id);
        self.when_to_use = normalize_string_list(self.when_to_use);
        self.preconditions = normalize_string_list(self.preconditions);
        self.primitive_steps = normalize_string_list(self.primitive_steps);
        self.done_criteria = normalize_string_list(self.done_criteria);
        self.recovery = normalize_string_list(self.recovery);

        if self.id.is_empty() {
            return Err(miette!("workflow.id cannot be empty"));
        }
        Ok(self)
    }

    pub fn primitive_summary(&self) -> PrimitiveSummary {
        PrimitiveSummary {
            id: self.id.clone(),
            origin: PrimitiveOrigin::Workspace,
            capability_summary: compact_list_summary(&self.primitive_steps, 2),
            inputs_summary: compact_list_summary(&self.preconditions, 2),
            outputs_summary: compact_list_summary(&self.done_criteria, 2),
            when_to_use_summary: compact_list_summary(&self.when_to_use, 1),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PrimitiveSummary {
    pub id: String,
    pub origin: PrimitiveOrigin,
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
pub struct PrimitiveRoutingCatalog {
    pub primitive_ids: Vec<PrimitiveId>,
    pub relevant_primitives: Vec<PrimitiveSummary>,
    pub total_count: usize,
    pub relevant_omitted_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PrimitiveId {
    pub id: String,
    pub origin: PrimitiveOrigin,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct NewPrimitiveSpec {
    pub id: String,
    #[serde(default)]
    pub when_to_use: Vec<String>,
    #[serde(default)]
    pub preconditions: Vec<String>,
    #[serde(default)]
    pub primitive_steps: Vec<String>,
    #[serde(default)]
    pub done_criteria: Vec<String>,
    #[serde(default)]
    pub recovery: Vec<String>,
}

impl NewPrimitiveSpec {
    pub fn into_workflow_spec(self) -> PrimitiveSpec {
        PrimitiveSpec {
            id: self.id,
            when_to_use: self.when_to_use,
            preconditions: self.preconditions,
            primitive_steps: self.primitive_steps,
            done_criteria: self.done_criteria,
            recovery: self.recovery,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PrimitiveRunRecord {
    pub run_id: String,
    pub workflow_id: String,
    pub started_at_ms: i64,
    pub ended_at_ms: i64,
    pub origin: String,
    pub outcome: PrimitiveRunOutcome,
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
pub enum PrimitiveRunOutcome {
    Completed,
    Blocked,
    Abandoned,
    Superseded,
    NoProgress,
}

pub struct PrimitiveRunBatch {
    pub records: Vec<PrimitiveRunRecord>,
    pub unread_record_count: usize,
    pub next_offset: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PrimitiveSpecPatch {
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

struct StoredPrimitive {
    spec: PrimitiveSpec,
    path: Option<PathBuf>,
    origin: PrimitiveOrigin,
}

pub struct PrimitiveStore {
    workflow_dir: PathBuf,
    workflows: BTreeMap<String, StoredPrimitive>,
}

impl PrimitiveStore {
    pub async fn new() -> Self {
        let workflow_dir = resolve_runtime_workspace_dir()
            .unwrap()
            .join(WORKFLOWS_DIR_NAME);
        Self::open_scoped(workflow_dir).await
    }

    pub(crate) async fn open_scoped(workflow_dir: PathBuf) -> Self {
        let mut store = Self {
            workflow_dir,
            workflows: load_builtin_primitives(),
        };
        store.load_from_disk().await;
        store
    }

    pub fn get(&self, workflow_id: &str) -> Option<&PrimitiveSpec> {
        self.workflows.get(workflow_id).map(|stored| &stored.spec)
    }

    pub fn workflow_origin(&self, workflow_id: &str) -> Option<PrimitiveOrigin> {
        self.workflows.get(workflow_id).map(|stored| stored.origin)
    }

    pub fn workflow_path(&self, workflow_id: &str) -> Option<&Path> {
        self.workflows
            .get(workflow_id)
            .and_then(|stored| stored.path.as_deref())
    }

    pub fn workspace_list(&self) -> Vec<PrimitiveSpec> {
        self.workflows
            .values()
            .filter(|stored| stored.origin == PrimitiveOrigin::Workspace)
            .map(|stored| stored.spec.clone())
            .collect()
    }

    pub fn primitive_routing_catalog(&self, query: &str, limit: usize) -> PrimitiveRoutingCatalog {
        let query_terms = workflow_relevance_terms(query);
        let mut primitive_ids = self
            .workflows
            .values()
            .map(|stored| PrimitiveId {
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
        PrimitiveRoutingCatalog {
            primitive_ids,
            relevant_primitives,
            total_count,
            relevant_omitted_count,
        }
    }

    pub fn activate_composed_primitive(&self, primitive_id: &str) -> Result<PrimitiveActivation> {
        let primitive_id = primitive_id.trim();
        if primitive_id.is_empty() {
            return Err(miette!(
                "activate_composed_primitive requires non-empty primitive_id"
            ));
        }
        if let Some(stored) = self.workflows.get(primitive_id) {
            return Ok(PrimitiveActivation::Single {
                primitive: stored.spec.clone(),
            });
        }

        let composition = self.compose_primitives(primitive_id)?;
        Ok(PrimitiveActivation::Composition { composition })
    }

    pub fn compose_primitives(&self, composition: &str) -> Result<PrimitiveComposition> {
        let primitive_ids = parse_primitive_composition(composition, |candidate| {
            self.workflows.contains_key(candidate)
        })?;
        Ok(PrimitiveComposition {
            composition_id: primitive_ids.join("-"),
            primitive_ids,
        })
    }

    pub async fn create_workflow(&mut self, draft: NewPrimitiveSpec) -> Result<PrimitiveSpec> {
        if draft.id.trim().is_empty() {
            return Err(miette!("create_primitive_spec requires non-empty id"));
        }
        if !is_valid_primitive_id(draft.id.trim()) {
            return Err(miette!(
                "primitive_id `{}` is invalid; primitive ids may only use lowercase a-z and '-'",
                draft.id
            ));
        }
        if draft.when_to_use.is_empty() {
            return Err(miette!(
                "create_primitive_spec requires at least one when_to_use item"
            ));
        }
        if draft.done_criteria.is_empty() {
            return Err(miette!(
                "create_primitive_spec requires at least one done_criteria item"
            ));
        }

        let spec = draft.into_workflow_spec().normalize()?;
        if self.workflows.contains_key(&spec.id) {
            return Err(miette!("primitive_id `{}` already exists", spec.id));
        }
        let path = self.workflow_dir.join(format!("{}.md", spec.id));
        write_workflow_file(&path, &spec).await?;
        self.workflows.insert(
            spec.id.clone(),
            StoredPrimitive {
                spec: spec.clone(),
                path: Some(path),
                origin: PrimitiveOrigin::Workspace,
            },
        );
        Ok(spec)
    }

    pub async fn apply_patch(&mut self, patch: PrimitiveSpecPatch) -> Result<PrimitiveSpec> {
        let stored = self
            .workflows
            .get_mut(&patch.workflow_id)
            .ok_or_else(|| miette!("unknown workflow_id `{}`", patch.workflow_id))?;
        if stored.origin != PrimitiveOrigin::Workspace {
            return Err(miette!(
                "builtin primitive `{}` is read-only and cannot be patched",
                patch.workflow_id
            ));
        }
        let path = stored.path.clone().ok_or_else(|| {
            miette!(
                "workspace primitive `{}` is missing backing path",
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
            &mut stored.spec.primitive_steps,
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
        replacement: PrimitiveSpec,
    ) -> Result<PrimitiveSpec> {
        let workflow_id = workflow_id.trim().to_string();
        if workflow_id.is_empty() {
            return Err(miette!("replace_workspace_workflow requires non-empty id"));
        }
        if !is_valid_primitive_id(&workflow_id) {
            return Err(miette!(
                "primitive_id `{workflow_id}` is invalid; primitive ids may only use lowercase a-z and '-'"
            ));
        }

        let stored = self
            .workflows
            .get_mut(&workflow_id)
            .ok_or_else(|| miette!("unknown primitive_id `{workflow_id}`"))?;
        if stored.origin != PrimitiveOrigin::Workspace {
            return Err(miette!(
                "builtin primitive `{workflow_id}` is read-only and cannot be updated"
            ));
        }
        let path = stored.path.clone().ok_or_else(|| {
            miette!("workspace primitive `{workflow_id}` is missing backing path")
        })?;

        if !is_valid_primitive_id(replacement.id.trim()) {
            return Err(miette!(
                "replacement primitive id `{}` is invalid; primitive ids may only use lowercase a-z and '-'",
                replacement.id
            ));
        }
        let replacement = replacement.normalize()?;
        if replacement.id != workflow_id {
            return Err(miette!(
                "replacement primitive id `{}` does not match target primitive_id `{workflow_id}`",
                replacement.id
            ));
        }
        if replacement.when_to_use.is_empty() {
            return Err(miette!(
                "update_primitive_spec requires at least one when_to_use item"
            ));
        }
        if replacement.primitive_steps.is_empty() {
            return Err(miette!(
                "update_primitive_spec requires at least one primitive_steps item"
            ));
        }
        if replacement.done_criteria.is_empty() {
            return Err(miette!(
                "update_primitive_spec requires at least one done_criteria item"
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
    ) -> Result<PrimitiveSpec> {
        if !self.workflows.contains_key(target_workflow_id) {
            return Err(miette!("unknown target workflow_id `{target_workflow_id}`"));
        }
        if self.workflow_origin(target_workflow_id) != Some(PrimitiveOrigin::Workspace) {
            return Err(miette!(
                "builtin primitive `{target_workflow_id}` is read-only and cannot be merged"
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
                    .filter(|stored| stored.origin == PrimitiveOrigin::Workspace)
                    .map(|stored| stored.spec.clone())
                    .ok_or_else(|| miette!("unknown source workflow_id `{source_id}`"))
            })
            .collect::<Result<Vec<_>>>()?;

        let target = self
            .workflows
            .get_mut(target_workflow_id)
            .ok_or_else(|| miette!("unknown target workflow_id `{target_workflow_id}`"))?;
        let target_path = target.path.clone().ok_or_else(|| {
            miette!("workspace primitive `{target_workflow_id}` is missing backing path")
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
                &mut target.spec.primitive_steps,
                normalize_string_list(source.primitive_steps.clone()),
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
            let Some(file_id) = primitive_id_from_path(&path) else {
                tracing::warn!(
                    "workspace primitive filename `{}` is invalid; primitive ids may only use lowercase a-z and '-'",
                    path.display()
                );
                continue;
            };
            let Ok(content) = tokio::fs::read_to_string(&path).await else {
                continue;
            };
            match parse_workflow_file(&content, Some(&file_id)) {
                Ok(spec) => {
                    if self.workflows.contains_key(&spec.id) {
                        tracing::warn!(
                            "workspace primitive id `{}` conflicts with existing builtin/workspace definition at {}; skipping",
                            spec.id,
                            path.display()
                        );
                        continue;
                    }
                    self.workflows.insert(
                        spec.id.clone(),
                        StoredPrimitive {
                            spec,
                            path: Some(path),
                            origin: PrimitiveOrigin::Workspace,
                        },
                    );
                }
                Err(err) => {
                    tracing::warn!(
                        "failed to parse primitive spec file {}: {err:?}",
                        path.display()
                    );
                }
            }
        }
    }
}

fn load_builtin_primitives() -> BTreeMap<String, StoredPrimitive> {
    let mut workflows = BTreeMap::new();
    for (source_name, content) in builtin_primitive_bindings::BUILTIN_PRIMITIVE_SOURCES {
        if !is_valid_primitive_id(source_name) {
            tracing::warn!(
                "builtin primitive filename `{source_name}` is invalid; primitive ids may only use lowercase a-z and '-'"
            );
            continue;
        }
        match parse_workflow_file(content, Some(source_name)) {
            Ok(spec) => {
                if workflows.contains_key(&spec.id) {
                    tracing::warn!(
                        "duplicate builtin primitive id `{}` detected in source {}; keeping first definition",
                        spec.id,
                        source_name
                    );
                    continue;
                }
                workflows.insert(
                    spec.id.clone(),
                    StoredPrimitive {
                        spec,
                        path: None,
                        origin: PrimitiveOrigin::Builtin,
                    },
                );
            }
            Err(err) => {
                tracing::warn!(
                    "failed to parse builtin primitive source {}: {err:?}",
                    source_name
                );
            }
        }
    }
    workflows
}

pub async fn load_primitive_run_batch() -> Result<PrimitiveRunBatch> {
    let workflow_run_records_io_guard = workflow_run_records_io_lock().lock().await;
    let path = workflow_run_records_file_path().await;
    let bytes = match fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            drop(workflow_run_records_io_guard);
            return Ok(PrimitiveRunBatch {
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
        let record: PrimitiveRunRecord = serde_json::from_str(line).map_err(|err| {
            miette!(
                "failed to parse workflow run record from {}: {err}",
                path.display()
            )
        })?;
        records.push(record);
    }
    let unread_record_count = records.len();
    drop(workflow_run_records_io_guard);
    Ok(PrimitiveRunBatch {
        records,
        unread_record_count,
        next_offset: offset,
    })
}

pub async fn primitive_run_record_count() -> Result<usize> {
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

pub async fn append_primitive_run_records(records: &[PrimitiveRunRecord]) -> Result<usize> {
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
            let record: PrimitiveRunRecord = serde_json::from_str(trimmed).map_err(|err| {
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

fn parse_workflow_file(content: &str, expected_id: Option<&str>) -> Result<PrimitiveSpec> {
    let content = normalize_workflow_line_endings(content);
    let body = strip_optional_frontmatter(content.as_ref());
    let sections = parse_markdown_sections(body);
    let Some(expected_id) = expected_id else {
        return Err(miette!(
            "parse_workflow_file requires filename primitive id"
        ));
    };
    let spec = PrimitiveSpec {
        id: expected_id.to_string(),
        when_to_use: parse_markdown_list(sections.get("When To Use")),
        preconditions: parse_markdown_list(sections.get("Preconditions")),
        primitive_steps: parse_markdown_list(sections.get("Workflow")),
        done_criteria: parse_markdown_list(sections.get("Done Criteria")),
        recovery: parse_markdown_list(sections.get("Recovery")),
    }
    .normalize()?;
    if spec.id != expected_id {
        return Err(miette!(
            "primitive filename id `{expected_id}` normalizes to `{}`, but primitive filenames must already use lowercase a-z and '-' only",
            spec.id
        ));
    }
    Ok(spec)
}

fn strip_optional_frontmatter(content: &str) -> &str {
    let Some(rest) = content.strip_prefix("---\n") else {
        return content;
    };
    rest.split_once("\n---\n")
        .map(|(_, body)| body)
        .unwrap_or(content)
}

fn normalize_workflow_line_endings(content: &str) -> Cow<'_, str> {
    if content.contains('\r') {
        Cow::Owned(content.replace("\r\n", "\n").replace('\r', "\n"))
    } else {
        Cow::Borrowed(content)
    }
}

async fn write_workflow_file(path: &Path, spec: &PrimitiveSpec) -> Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|err| {
            miette!(
                "create primitive spec directory {} failed: {err}",
                parent.display()
            )
        })?;
    }

    let body = render_workflow_markdown_body(spec);
    write_bytes_atomic(
        path.to_path_buf(),
        body.into_bytes(),
        PersistenceFileMode::Default,
    )
    .await
    .map_err(|err| miette!("write primitive spec file {} failed: {err}", path.display()))
}

fn render_workflow_markdown_body(spec: &PrimitiveSpec) -> String {
    [
        render_section("When To Use", &spec.when_to_use, false),
        render_section("Preconditions", &spec.preconditions, false),
        render_section("Workflow", &spec.primitive_steps, true),
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

fn workflow_content_equal(left: &PrimitiveSpec, right: &PrimitiveSpec) -> bool {
    left.id == right.id
        && left.when_to_use == right.when_to_use
        && left.preconditions == right.preconditions
        && left.primitive_steps == right.primitive_steps
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

fn workflow_route_score(spec: &PrimitiveSpec, query_terms: &[String]) -> usize {
    if query_terms.is_empty() {
        return 0;
    }
    let candidate = workflow_relevance_text(spec);
    query_terms
        .iter()
        .filter(|term| candidate.contains(term.as_str()))
        .count()
}

fn workflow_relevance_text(spec: &PrimitiveSpec) -> String {
    let mut parts = vec![spec.id.replace(['-', '_'], " ")];
    parts.extend(spec.when_to_use.iter().cloned());
    parts.extend(spec.preconditions.iter().cloned());
    parts.extend(spec.primitive_steps.iter().cloned());
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

fn workflow_origin_sort_key(origin: PrimitiveOrigin) -> u8 {
    match origin {
        PrimitiveOrigin::Builtin => 0,
        PrimitiveOrigin::Workspace => 1,
    }
}

fn normalize_identifier(value: &str) -> String {
    let mut normalized = String::new();
    let mut previous_was_dash = true;
    for ch in value.trim().chars() {
        if ch.is_ascii_alphabetic() {
            normalized.push(ch.to_ascii_lowercase());
            previous_was_dash = false;
        } else if (ch == '-' || ch.is_whitespace() || ch == '_') && !previous_was_dash {
            normalized.push('-');
            previous_was_dash = true;
        }
    }
    if previous_was_dash {
        normalized.pop();
    }
    normalized
}

fn is_valid_primitive_id(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('-')
        && !value.ends_with('-')
        && value.chars().all(|ch| ch.is_ascii_lowercase() || ch == '-')
}

fn primitive_id_from_path(path: &Path) -> Option<String> {
    let file_id = path.file_stem()?.to_str()?;
    is_valid_primitive_id(file_id).then(|| file_id.to_string())
}

fn parse_primitive_composition(
    composition: &str,
    exists: impl Fn(&str) -> bool,
) -> Result<Vec<String>> {
    let composition = composition.trim();
    if composition.is_empty() {
        return Err(miette!(
            "activate_composed_primitive requires non-empty primitive_id"
        ));
    }
    if !is_valid_primitive_id(composition) {
        return Err(miette!(
            "primitive composition `{composition}` is invalid; use only lowercase a-z and '-'"
        ));
    }

    fn segment<F: Fn(&str) -> bool>(
        parts: &[&str],
        index: usize,
        exists: &F,
    ) -> Option<Vec<String>> {
        if index == parts.len() {
            return Some(Vec::new());
        }

        for end in (index + 1..=parts.len()).rev() {
            let candidate = parts[index..end].join("-");
            if !exists(&candidate) {
                continue;
            }
            if let Some(mut remainder) = segment(parts, end, exists) {
                let mut primitive_ids = Vec::with_capacity(remainder.len() + 1);
                primitive_ids.push(candidate);
                primitive_ids.append(&mut remainder);
                return Some(primitive_ids);
            }
        }
        None
    }

    let parts = composition.split('-').collect::<Vec<_>>();
    let Some(primitive_ids) = segment(&parts, 0, &exists) else {
        return Err(miette!(
            "primitive composition `{composition}` cannot be segmented into existing primitives"
        ));
    };

    if primitive_ids.len() < 2 {
        return Err(miette!(
            "activate_composed_primitive composition requires at least two existing primitive ids joined by '-'"
        ));
    }
    Ok(primitive_ids)
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
        .join(PRIMITIVE_RUN_RECORDS_FILE_NAME)
}

fn workflow_run_records_io_lock() -> &'static tokio::sync::Mutex<()> {
    PRIMITIVE_RUN_RECORDS_IO_LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn create_workflow_writes_primitive_markdown_and_can_reload() {
        let temp_dir = TempDir::new().expect("create primitive temp dir");
        let primary = temp_dir.path().join("workflows");
        let mut store = PrimitiveStore::open_scoped(primary.clone()).await;
        let created = store
            .create_workflow(NewPrimitiveSpec {
                id: "repair-flaky-test-pipeline".to_string(),
                when_to_use: vec!["flaky test failure".to_string()],
                preconditions: vec!["failing logs available".to_string()],
                primitive_steps: vec!["collect evidence".to_string(), "verify fix".to_string()],
                done_criteria: vec!["result is stable".to_string()],
                recovery: vec!["return to previous stable state".to_string()],
            })
            .await
            .expect("create primitive spec");

        assert_eq!(created.id, "repair-flaky-test-pipeline");
        assert!(primary.join("repair-flaky-test-pipeline.md").exists());

        let reloaded = PrimitiveStore::open_scoped(primary.clone()).await;
        let loaded = reloaded
            .get("repair-flaky-test-pipeline")
            .expect("reloaded primitive spec");
        assert_eq!(loaded.primitive_steps.len(), 2);
    }

    #[tokio::test]
    async fn primitive_routing_catalog_includes_all_ids_and_ranks_relevant_primitives() {
        let temp_dir = TempDir::new().expect("create primitive temp dir");
        let primary = temp_dir.path().join("workflows");
        let mut store = PrimitiveStore::open_scoped(primary).await;
        store
            .create_workflow(NewPrimitiveSpec {
                id: "zephyr-quartz-inspection".to_string(),
                when_to_use: vec!["inspect zephyr quartz artifacts".to_string()],
                preconditions: vec!["artifact path is known".to_string()],
                primitive_steps: vec!["inspect artifact metadata".to_string()],
                done_criteria: vec!["artifact findings are summarized".to_string()],
                recovery: vec![],
            })
            .await
            .expect("create matching primitive spec");
        store
            .create_workflow(NewPrimitiveSpec {
                id: "unrelated-ritual".to_string(),
                when_to_use: vec!["handle unrelated ritual tasks".to_string()],
                preconditions: vec![],
                primitive_steps: vec!["perform unrelated step".to_string()],
                done_criteria: vec!["unrelated result exists".to_string()],
                recovery: vec![],
            })
            .await
            .expect("create unrelated primitive spec");

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

    #[tokio::test]
    async fn compose_primitives_returns_existing_filename_sequence() {
        let temp_dir = TempDir::new().expect("create primitive temp dir");
        let primary = temp_dir.path().join("workflows");
        let mut store = PrimitiveStore::open_scoped(primary).await;
        store
            .create_workflow(NewPrimitiveSpec {
                id: "alpha-project-scan".to_string(),
                when_to_use: vec!["inspect project".to_string()],
                preconditions: vec![],
                primitive_steps: vec!["inspect files".to_string()],
                done_criteria: vec!["findings exist".to_string()],
                recovery: vec![],
            })
            .await
            .expect("create inspect primitive");
        store
            .create_workflow(NewPrimitiveSpec {
                id: "beta-required-checks".to_string(),
                when_to_use: vec!["run checks".to_string()],
                preconditions: vec![],
                primitive_steps: vec!["run tests".to_string()],
                done_criteria: vec!["checks pass".to_string()],
                recovery: vec![],
            })
            .await
            .expect("create checks primitive");

        let composition = store
            .compose_primitives("alpha-project-scan-beta-required-checks")
            .expect("compose primitives");

        assert_eq!(
            composition.composition_id,
            "alpha-project-scan-beta-required-checks"
        );
        assert_eq!(
            composition.primitive_ids,
            vec!["alpha-project-scan", "beta-required-checks"]
        );
    }

    #[tokio::test]
    async fn compose_primitives_rejects_non_filename_input() {
        let temp_dir = TempDir::new().expect("create primitive temp dir");
        let primary = temp_dir.path().join("workflows");
        let store = PrimitiveStore::open_scoped(primary).await;

        let err = store
            .compose_primitives("inspect_local_project-run-required-checks")
            .expect_err("underscores are not legal primitive filename syntax");

        assert!(err.to_string().contains("use only lowercase a-z and '-'"));
    }

    #[test]
    fn primitive_summary_exposes_primitive_io_contract() {
        let summary = PrimitiveSpec {
            id: "modify-local-project".to_string(),
            when_to_use: vec!["local project needs edits".to_string()],
            preconditions: vec!["project path is known".to_string()],
            primitive_steps: vec![
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
    async fn builtin_primitive_ids_cannot_be_overwritten() {
        let temp_dir = TempDir::new().expect("create primitive temp dir");
        let primary = temp_dir.path().join("workflows");
        let mut store = PrimitiveStore::open_scoped(primary).await;

        let err = store
            .create_workflow(NewPrimitiveSpec {
                id: "author-workspace-app".to_string(),
                when_to_use: vec!["test".to_string()],
                preconditions: vec![],
                primitive_steps: vec!["step".to_string()],
                done_criteria: vec!["done".to_string()],
                recovery: vec![],
            })
            .await
            .expect_err("builtin primitive id should be reserved");

        assert!(err.to_string().contains("already exists"));
    }

    #[tokio::test]
    async fn builtin_primitives_are_read_only() {
        let temp_dir = TempDir::new().expect("create primitive temp dir");
        let primary = temp_dir.path().join("workflows");
        let mut store = PrimitiveStore::open_scoped(primary).await;

        let err = store
            .apply_patch(PrimitiveSpecPatch {
                workflow_id: "author-workspace-app".to_string(),
                when_to_use_additions: vec!["extra".to_string()],
                precondition_additions: Vec::new(),
                workflow_step_additions: Vec::new(),
                done_criteria_additions: Vec::new(),
                recovery_additions: Vec::new(),
            })
            .await
            .expect_err("builtin primitive patch should be rejected");

        assert!(err.to_string().contains("read-only"));
    }

    #[tokio::test]
    async fn replace_workspace_primitive_spec_rewrites_complete_spec() {
        let temp_dir = TempDir::new().expect("create primitive temp dir");
        let primary = temp_dir.path().join("workflows");
        let mut store = PrimitiveStore::open_scoped(primary.clone()).await;
        store
            .create_workflow(NewPrimitiveSpec {
                id: "search-todays-news".to_string(),
                when_to_use: vec!["search current news".to_string()],
                preconditions: vec!["network is available".to_string()],
                primitive_steps: vec![
                    "search aggregator".to_string(),
                    "repeat fallback searches".to_string(),
                ],
                done_criteria: vec!["sent news summary".to_string()],
                recovery: vec!["keep searching".to_string()],
            })
            .await
            .expect("create primitive");

        let updated = store
            .replace_workspace_workflow(
                "search-todays-news",
                PrimitiveSpec {
                    id: "search-todays-news".to_string(),
                    when_to_use: vec!["user asks for today's news".to_string()],
                    preconditions: vec!["date scope is known".to_string()],
                    primitive_steps: vec![
                        "choose approved news sources".to_string(),
                        "verify publication dates".to_string(),
                        "send concise summary".to_string(),
                    ],
                    done_criteria: vec!["summary cites sources".to_string()],
                    recovery: vec!["return a limited summary if sources are sparse".to_string()],
                },
            )
            .await
            .expect("replace primitive spec");

        assert_eq!(updated.primitive_steps.len(), 3);
        assert!(
            !updated
                .primitive_steps
                .iter()
                .any(|step| step == "repeat fallback searches")
        );

        let reloaded = PrimitiveStore::open_scoped(primary).await;
        let loaded = reloaded
            .get("search-todays-news")
            .expect("reloaded updated primitive spec");
        assert_eq!(
            loaded.primitive_steps,
            vec![
                "choose approved news sources",
                "verify publication dates",
                "send concise summary"
            ]
        );
    }

    #[tokio::test]
    async fn replace_rejects_builtin_primitive() {
        let temp_dir = TempDir::new().expect("create primitive temp dir");
        let primary = temp_dir.path().join("workflows");
        let mut store = PrimitiveStore::open_scoped(primary).await;

        let err = store
            .replace_workspace_workflow(
                "author-workspace-app",
                PrimitiveSpec {
                    id: "author-workspace-app".to_string(),
                    when_to_use: vec!["test".to_string()],
                    preconditions: vec![],
                    primitive_steps: vec!["step".to_string()],
                    done_criteria: vec!["done".to_string()],
                    recovery: vec![],
                },
            )
            .await
            .expect_err("builtin primitive update should be rejected");

        assert!(err.to_string().contains("read-only"));
    }

    #[test]
    fn parse_workflow_file_accepts_crlf_line_endings() {
        let spec = parse_workflow_file(
            "---\r\nid: ignored-frontmatter\r\n---\r\n\r\n## When To Use\r\n- Windows checkout\r\n\r\n## Preconditions\r\n- A primitive spec file uses CRLF\r\n\r\n## Workflow\r\n1. Parse frontmatter\r\n\r\n## Done Criteria\r\n- Primitive spec is loaded\r\n\r\n## Recovery\r\n- Retry with normalized line endings\r\n",
            Some("crlf-workflow"),
        )
        .expect("parse CRLF workflow");

        assert_eq!(spec.id, "crlf-workflow");
        assert_eq!(spec.primitive_steps, vec!["Parse frontmatter"]);
        assert_eq!(spec.done_criteria, vec!["Primitive spec is loaded"]);
    }

    #[test]
    fn parse_workflow_file_uses_filename_id_over_frontmatter() {
        let spec = parse_workflow_file(
            "---\nid: arbitrary-content-id\nnotes: unrestricted\n---\n\n## When To Use\n- Any markdown content may differ from filename identity\n\n## Workflow\n- Load body\n\n## Done Criteria\n- Filename identity wins\n",
            Some("filename-identity"),
        )
        .expect("parse primitive spec with arbitrary frontmatter");

        assert_eq!(spec.id, "filename-identity");
        assert_eq!(spec.primitive_steps, vec!["Load body"]);
    }

    #[tokio::test]
    async fn merge_primitives_deletes_sources_and_updates_target() {
        let temp_dir = TempDir::new().expect("create primitive temp dir");
        let primary = temp_dir.path().join("workflows");
        let mut store = PrimitiveStore::open_scoped(primary.clone()).await;
        let target = store
            .create_workflow(NewPrimitiveSpec {
                id: "investigate-runtime-failure".to_string(),
                when_to_use: vec!["runtime failure".to_string()],
                preconditions: vec![],
                primitive_steps: vec!["collect logs".to_string()],
                done_criteria: vec!["cause is clear".to_string()],
                recovery: vec![],
            })
            .await
            .expect("create target");
        let source = store
            .create_workflow(NewPrimitiveSpec {
                id: "investigate-runtime-errors".to_string(),
                when_to_use: vec!["runtime error".to_string()],
                preconditions: vec![],
                primitive_steps: vec!["locate root cause".to_string()],
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
            .expect("merge primitives");

        assert!(
            merged
                .primitive_steps
                .iter()
                .any(|item| item == "locate root cause")
        );
        assert!(store.get(&source.id).is_none());
        assert!(!primary.join(format!("{}.md", source.id)).exists());
    }

    #[tokio::test]
    async fn workflow_run_records_are_appended_and_deduped() {
        let temp_dir = TempDir::new().expect("workflow run record temp dir");
        let _home_override = crate::DaatLocusHomeOverride::set(temp_dir.path().to_path_buf()).await;

        let record = PrimitiveRunRecord {
            run_id: "workflow-run:run-1".to_string(),
            workflow_id: "repair-flaky-test-pipeline".to_string(),
            started_at_ms: 1,
            ended_at_ms: 2,
            origin: "event:test".to_string(),
            outcome: PrimitiveRunOutcome::Completed,
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

        append_primitive_run_records(&[record.clone(), record.clone()])
            .await
            .expect("append workflow run records");
        let batch = load_primitive_run_batch()
            .await
            .expect("load workflow run records");
        let count = primitive_run_record_count()
            .await
            .expect("count workflow run records");
        append_primitive_run_records(&[later_record.clone()])
            .await
            .expect("append later workflow run record");
        compact_workflow_run_record_file(batch.next_offset)
            .await
            .expect("compact consumed workflow run records");
        let remaining_batch = load_primitive_run_batch()
            .await
            .expect("load remaining workflow run records");
        let remaining_count = primitive_run_record_count()
            .await
            .expect("count remaining workflow run records");

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
