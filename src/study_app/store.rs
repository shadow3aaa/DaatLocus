//! SQLite-backed knowledge graph storage for Study Mode.
//!
//! The store owns modules, nodes, typed relations, per-node progress, the
//! question bank, and attempt history. Every operation validates the Study
//! invariants that code can enforce mechanically. Semantic judgments, such as
//! whether two concepts are the same node, remain the model's responsibility.

use std::{collections::BTreeMap, path::PathBuf};

use miette::{IntoDiagnostic, Result, miette};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

pub const STUDY_RELATIONS: &[&str] = &[
    "prerequisite",
    "part_of",
    "related",
    "contrast",
    "example_of",
    "applies_to",
];

pub const QUESTION_DIFFICULTIES: &[&str] = &["easy", "medium", "hard"];

pub const ATTEMPT_OUTCOMES: &[&str] = &["correct", "partial", "incorrect"];

const STUDY_DB_FILE_NAME: &str = "graph.sqlite3";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StudyModule {
    pub id: String,
    pub title: String,
    pub description: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StudySource {
    pub title: String,
    pub url: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StudyNode {
    pub id: String,
    pub module_id: String,
    pub title: String,
    pub summary: String,
    pub body: String,
    pub aliases: Vec<String>,
    pub tags: Vec<String>,
    pub sources: Vec<StudySource>,
    pub content_version: i64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StudyProgress {
    /// Understanding level as a percentage in 0..=100.
    pub understanding: i64,
    pub evidence: String,
    pub updated_by: String,
    pub updated_at_ms: i64,
}

impl Default for StudyProgress {
    fn default() -> Self {
        Self {
            understanding: 0,
            evidence: String::new(),
            updated_by: "code".to_string(),
            updated_at_ms: 0,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StudyNodeSummary {
    pub id: String,
    pub module_id: String,
    pub title: String,
    pub summary: String,
    pub aliases: Vec<String>,
    pub tags: Vec<String>,
    pub content_version: i64,
    pub progress: StudyProgress,
    pub question_count: usize,
    pub stale_question_count: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StudyEdge {
    pub id: i64,
    pub from: String,
    pub to: String,
    pub relation: String,
    pub note: String,
}

/// Understanding at or above this percentage counts as mastered.
pub const STUDY_MASTERED_THRESHOLD: i64 = 90;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct StudyStats {
    pub module_count: usize,
    pub node_count: usize,
    pub edge_count: usize,
    pub mastered_count: usize,
    pub in_progress_count: usize,
    pub unseen_count: usize,
    pub average_understanding: i64,
    pub question_count: usize,
    pub stale_question_count: usize,
    pub attempt_count: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct StudyMaintenance {
    pub orphan_node_ids: Vec<String>,
    pub empty_module_ids: Vec<String>,
    pub duplicate_candidate_groups: Vec<Vec<String>>,
    pub unlinked_node_ids: Vec<String>,
    pub stale_question_count: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StudyModuleSummary {
    pub module: StudyModule,
    pub node_count: usize,
    pub mastered_count: usize,
    pub in_progress_count: usize,
    pub unseen_count: usize,
    pub average_understanding: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StudyGraphSnapshot {
    pub generated_at_ms: i64,
    pub revision: i64,
    pub modules: Vec<StudyModuleSummary>,
    pub nodes: Vec<StudyNodeSummary>,
    pub edges: Vec<StudyEdge>,
    pub stats: StudyStats,
    pub maintenance: StudyMaintenance,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StudyQuestion {
    pub id: String,
    pub node_id: String,
    pub question: String,
    pub answer: String,
    pub difficulty: String,
    pub node_content_version: i64,
    pub is_stale: bool,
    pub created_at_ms: i64,
    pub last_outcome: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StudyNeighbor {
    pub node: StudyNodeSummary,
    pub relation: String,
    pub direction: String,
    pub note: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StudyNodeDetail {
    pub node: StudyNode,
    pub progress: StudyProgress,
    pub questions: Vec<StudyQuestion>,
    pub neighbors: Vec<StudyNeighbor>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StudyFindSimilar {
    pub candidates: Vec<StudyNodeSummary>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StudyCreateNodeResult {
    pub node: StudyNode,
    pub similar: Vec<StudyNodeSummary>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StudyMergeResult {
    pub canonical_node_id: String,
    pub merged_node_id: String,
    pub canonical: StudyNode,
    pub moved_edges: usize,
    pub merged_aliases: Vec<String>,
    pub moved_questions: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StudyLinkResult {
    pub edge: StudyEdge,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StudyRemoveEdgeResult {
    pub removed_edges: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StudyAttemptResult {
    pub attempt_id: String,
    pub question_id: String,
    pub node_id: String,
    pub outcome: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StudyProgressUpdate {
    pub node_id: String,
    pub understanding: i64,
    pub evidence: String,
    pub updated_by: String,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StudyNodeUpdateResult {
    pub node: StudyNode,
    pub content_changed: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StudyQuestionWriteResult {
    pub added_questions: Vec<StudyQuestion>,
    pub stale_question_count: usize,
}

/// Where an operation runs relative to the caller. `user` means the user asked
/// for this change through the client surface; `agent` means the study agent
/// made the semantic decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StudyWriteOrigin {
    User,
    Agent,
}

impl StudyWriteOrigin {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Agent => "agent",
        }
    }
}

#[derive(Clone, Debug)]
pub struct StudyStore {
    db_path: PathBuf,
    fts_enabled: bool,
    revision: std::sync::Arc<std::sync::atomic::AtomicI64>,
}

#[derive(Clone, Debug, Default)]
pub struct CreateNodeInput {
    pub module_id: String,
    pub title: String,
    pub summary: String,
    pub body: String,
    pub aliases: Vec<String>,
    pub tags: Vec<String>,
    pub sources: Vec<StudySource>,
}

#[derive(Clone, Debug, Default)]
pub struct UpdateNodeInput {
    pub title: Option<String>,
    pub summary: Option<String>,
    pub body: Option<String>,
    pub aliases: Option<Vec<String>>,
    pub tags: Option<Vec<String>>,
    pub sources: Option<Vec<StudySource>>,
}

#[derive(Clone, Debug, Default)]
pub struct NewQuestionInput {
    pub question: String,
    pub answer: String,
    pub difficulty: String,
}

impl StudyStore {
    pub async fn open_default() -> Result<Self> {
        let root = crate::daat_locus_paths::daat_locus_paths()
            .await
            .root()
            .to_path_buf();
        let dir = root.join("study");
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|err| miette!("failed to create study directory {}: {err}", dir.display()))?;
        Self::open_at(dir.join(STUDY_DB_FILE_NAME))
    }

    pub fn open_at(db_path: PathBuf) -> Result<Self> {
        let store = Self {
            db_path,
            fts_enabled: false,
            revision: std::sync::Arc::new(std::sync::atomic::AtomicI64::new(0)),
        };
        let connection = store.open_connection()?;
        let fts_enabled = migrate(&connection)?;
        Ok(Self {
            fts_enabled,
            ..store
        })
    }

    fn open_connection(&self) -> Result<Connection> {
        Connection::open(&self.db_path).into_diagnostic()
    }

    /// Monotonic revision of the graph data. It changes on every successful
    /// mutation so clients can detect when to refetch.
    pub fn revision(&self) -> i64 {
        self.revision.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn bump_revision(&self) {
        self.revision
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn create_module(
        &self,
        title: &str,
        description: &str,
        origin: StudyWriteOrigin,
    ) -> Result<StudyModule> {
        let title = title.trim();
        if title.is_empty() {
            return Err(miette!("module title cannot be empty"));
        }
        let now = now_ms();
        let module = StudyModule {
            id: format!("module-{}", uuid::Uuid::new_v4()),
            title: title.to_string(),
            description: description.trim().to_string(),
            created_at_ms: now,
            updated_at_ms: now,
        };
        let connection = self.open_connection()?;
        connection
            .execute(
                "INSERT INTO study_modules (id, title, description, created_at_ms, updated_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    module.id,
                    module.title,
                    module.description,
                    module.created_at_ms,
                    module.updated_at_ms
                ],
            )
            .into_diagnostic()
            .map_err(|err| miette!("insert module failed: {err}"))?;
        let _ = origin;
        self.bump_revision();
        Ok(module)
    }

    pub fn list_modules(&self) -> Result<Vec<StudyModuleSummary>> {
        let connection = self.open_connection()?;
        let mut statement = connection
            .prepare(
                "SELECT m.id, m.title, m.description, m.created_at_ms, m.updated_at_ms,
                        n.id, COALESCE(p.understanding, 0)
                 FROM study_modules m
                 LEFT JOIN study_nodes n ON n.module_id = m.id
                 LEFT JOIN study_progress p ON p.node_id = n.id
                 ORDER BY m.created_at_ms ASC, m.id ASC",
            )
            .into_diagnostic()?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    StudyModule {
                        id: row.get(0)?,
                        title: row.get(1)?,
                        description: row.get(2)?,
                        created_at_ms: row.get(3)?,
                        updated_at_ms: row.get(4)?,
                    },
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            })
            .into_diagnostic()?;

        let mut modules: Vec<StudyModuleSummary> = Vec::new();
        let mut understanding_totals: Vec<(i64, usize)> = Vec::new();
        for row in rows {
            let (module, node_id, understanding) = row.into_diagnostic()?;
            let index = modules
                .iter()
                .position(|item| item.module.id == module.id)
                .unwrap_or_else(|| {
                    modules.push(StudyModuleSummary {
                        module,
                        node_count: 0,
                        mastered_count: 0,
                        in_progress_count: 0,
                        unseen_count: 0,
                        average_understanding: 0,
                    });
                    understanding_totals.push((0, 0));
                    modules.len() - 1
                });
            if node_id.is_none() {
                continue;
            }
            apply_understanding(&mut modules[index], understanding);
            understanding_totals[index].0 += understanding;
            understanding_totals[index].1 += 1;
        }
        for (index, (total, count)) in understanding_totals.iter().enumerate() {
            modules[index].average_understanding = if *count == 0 {
                0
            } else {
                total / *count as i64
            };
        }
        Ok(modules)
    }

    pub fn graph_snapshot(&self) -> Result<StudyGraphSnapshot> {
        let connection = self.open_connection()?;
        let modules = self.list_modules()?;
        let nodes = load_node_summaries(&connection)?;
        let edges = load_edges(&connection)?;
        let question_count = count_rows(&connection, "study_questions")?;
        let stale_question_count = stale_question_count(&connection)?;
        let attempt_count = count_rows(&connection, "study_attempts")?;

        let mut stats = StudyStats {
            module_count: modules.len(),
            node_count: nodes.len(),
            edge_count: edges.len(),
            question_count,
            stale_question_count,
            attempt_count,
            ..StudyStats::default()
        };
        let mut understanding_total = 0i64;
        for node in &nodes {
            let understanding = node.progress.understanding;
            understanding_total += understanding;
            if understanding >= STUDY_MASTERED_THRESHOLD {
                stats.mastered_count += 1;
            } else if understanding > 0 {
                stats.in_progress_count += 1;
            } else {
                stats.unseen_count += 1;
            }
        }
        stats.average_understanding = if nodes.is_empty() {
            0
        } else {
            understanding_total / nodes.len() as i64
        };

        let maintenance = load_maintenance(&connection, &modules, &nodes)?;
        Ok(StudyGraphSnapshot {
            generated_at_ms: now_ms(),
            revision: self.revision(),
            modules,
            nodes,
            edges,
            stats,
            maintenance,
        })
    }

    pub fn node_detail(&self, node_id: &str) -> Result<StudyNodeDetail> {
        let connection = self.open_connection()?;
        let node = load_node(&connection, node_id)?;
        let progress = load_progress(&connection, node_id)?;
        let questions = load_questions(&connection, node_id, node.content_version)?;
        let neighbors = load_neighbors(&connection, &node)?;
        Ok(StudyNodeDetail {
            node,
            progress,
            questions,
            neighbors,
        })
    }

    pub fn search_nodes(
        &self,
        query: &str,
        module_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<StudyNodeSummary>> {
        let query = query.trim();
        let limit = limit.clamp(1, 100);
        let connection = self.open_connection()?;
        let mut matched_ids = Vec::new();
        if self.fts_enabled && !query.is_empty() {
            matched_ids = fts_search(&connection, query, limit)?;
        }
        if matched_ids.is_empty() {
            matched_ids = like_search(&connection, query, limit)?;
        }

        let mut nodes = load_node_summaries(&connection)?;
        if let Some(module_id) = module_id {
            nodes.retain(|node| node.module_id == module_id);
        }
        if query.is_empty() {
            nodes.truncate(limit);
            return Ok(nodes);
        }
        nodes.retain(|node| {
            matched_ids.iter().any(|id| id == &node.id)
                || node.title.to_lowercase().contains(&query.to_lowercase())
        });
        Ok(nodes)
    }

    pub fn find_similar(&self, needle: &str, limit: usize) -> Result<Vec<StudyNodeSummary>> {
        let normalized = normalize_identity(needle);
        if normalized.is_empty() {
            return Ok(Vec::new());
        }
        let connection = self.open_connection()?;
        let nodes = load_node_summaries(&connection)?;
        let mut candidates: Vec<StudyNodeSummary> = nodes
            .into_iter()
            .filter(|node| {
                normalize_identity(&node.title) == normalized
                    || node
                        .aliases
                        .iter()
                        .any(|alias| normalize_identity(alias) == normalized)
            })
            .collect();
        if candidates.is_empty() {
            candidates = self.search_nodes(needle, None, limit)?;
        }
        candidates.truncate(limit.clamp(1, 20));
        Ok(candidates)
    }

    pub fn create_node(
        &self,
        input: &CreateNodeInput,
        origin: StudyWriteOrigin,
    ) -> Result<StudyCreateNodeResult> {
        let title = input.title.trim();
        if title.is_empty() {
            return Err(miette!("node title cannot be empty"));
        }
        if input.sources.is_empty() {
            return Err(miette!(
                "new study nodes require at least one source entry (title + url)"
            ));
        }
        let now = now_ms();
        let node = StudyNode {
            id: format!("node-{}", uuid::Uuid::new_v4()),
            module_id: input.module_id.clone(),
            title: title.to_string(),
            summary: input.summary.trim().to_string(),
            body: input.body.trim().to_string(),
            aliases: normalize_string_list(&input.aliases),
            tags: normalize_string_list(&input.tags),
            sources: input.sources.clone(),
            content_version: 1,
            created_at_ms: now,
            updated_at_ms: now,
        };

        let mut connection = self.open_connection()?;
        let transaction = connection.transaction().into_diagnostic()?;
        let module_exists: bool = transaction
            .query_row(
                "SELECT 1 FROM study_modules WHERE id = ?1",
                params![node.module_id],
                |_| Ok(true),
            )
            .optional()
            .into_diagnostic()?
            .unwrap_or(false);
        if !module_exists {
            return Err(miette!(
                "module `{}` does not exist; create the module first or reuse an existing module id",
                node.module_id
            ));
        }
        transaction
            .execute(
                "INSERT INTO study_nodes
                 (id, module_id, title, summary, body, aliases, tags, sources, content_version, created_at_ms, updated_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    node.id,
                    node.module_id,
                    node.title,
                    node.summary,
                    node.body,
                    encode_string_list(&node.aliases),
                    encode_string_list(&node.tags),
                    encode_sources(&node.sources),
                    node.content_version,
                    node.created_at_ms,
                    node.updated_at_ms,
                ],
            )
            .into_diagnostic()
            .map_err(|err| miette!("insert node failed: {err}"))?;
        transaction
            .execute(
                "INSERT INTO study_progress (node_id, understanding, evidence, updated_by, updated_at_ms)
                 VALUES (?1, 0, ?2, ?3, ?4)",
                params![node.id, "created", origin.as_str(), now],
            )
            .into_diagnostic()?;
        transaction.commit().into_diagnostic()?;

        let similar = self.find_similar(title, 5)?;
        let similar = similar
            .into_iter()
            .filter(|candidate| candidate.id != node.id)
            .collect();
        self.bump_revision();
        Ok(StudyCreateNodeResult { node, similar })
    }

    pub fn update_node(
        &self,
        node_id: &str,
        input: &UpdateNodeInput,
    ) -> Result<StudyNodeUpdateResult> {
        let connection = self.open_connection()?;
        let mut node = load_node(&connection, node_id)?;

        let mut content_changed = false;
        if let Some(title) = input.title.as_deref() {
            let title = title.trim();
            if title.is_empty() {
                return Err(miette!("node title cannot be empty"));
            }
            if title != node.title {
                node.title = title.to_string();
                content_changed = true;
            }
        }
        set_content_field(
            &mut node.summary,
            input.summary.as_deref(),
            &mut content_changed,
        );
        set_content_field(&mut node.body, input.body.as_deref(), &mut content_changed);
        if let Some(aliases) = input.aliases.as_ref() {
            let aliases = normalize_string_list(aliases);
            if aliases != node.aliases {
                node.aliases = aliases;
                content_changed = true;
            }
        }
        if let Some(tags) = input.tags.as_ref() {
            node.tags = normalize_string_list(tags);
        }
        if let Some(sources) = input.sources.as_ref() {
            if sources.is_empty() {
                return Err(miette!("node sources cannot become empty"));
            }
            node.sources = sources.clone();
        }
        if content_changed {
            node.content_version += 1;
        }
        node.updated_at_ms = now_ms();

        connection
            .execute(
                "UPDATE study_nodes
                 SET title = ?2, summary = ?3, body = ?4, aliases = ?5, tags = ?6, sources = ?7,
                     content_version = ?8, updated_at_ms = ?9
                 WHERE id = ?1",
                params![
                    node.id,
                    node.title,
                    node.summary,
                    node.body,
                    encode_string_list(&node.aliases),
                    encode_string_list(&node.tags),
                    encode_sources(&node.sources),
                    node.content_version,
                    node.updated_at_ms,
                ],
            )
            .into_diagnostic()
            .map_err(|err| miette!("update node failed: {err}"))?;
        self.bump_revision();
        Ok(StudyNodeUpdateResult {
            node,
            content_changed,
        })
    }

    pub fn link_nodes(
        &self,
        from_node_id: &str,
        to_node_id: &str,
        relation: &str,
        note: &str,
    ) -> Result<StudyLinkResult> {
        if !STUDY_RELATIONS.contains(&relation) {
            return Err(miette!(
                "unsupported relation `{relation}`; supported relations: {}",
                STUDY_RELATIONS.join(", ")
            ));
        }
        if from_node_id == to_node_id {
            return Err(miette!("cannot link a node to itself"));
        }
        let connection = self.open_connection()?;
        ensure_node_exists(&connection, from_node_id)?;
        ensure_node_exists(&connection, to_node_id)?;
        let existing: Option<i64> = connection
            .query_row(
                "SELECT id FROM study_edges WHERE from_node_id = ?1 AND to_node_id = ?2 AND relation = ?3",
                params![from_node_id, to_node_id, relation],
                |row| row.get(0),
            )
            .optional()
            .into_diagnostic()?;
        if existing.is_some() {
            return Err(miette!(
                "relation `{relation}` from `{from_node_id}` to `{to_node_id}` already exists"
            ));
        }
        if relation == "prerequisite"
            && prerequisite_path_exists(&connection, to_node_id, from_node_id)?
        {
            return Err(miette!(
                "link would create a prerequisite cycle: `{to_node_id}` already depends on `{from_node_id}`"
            ));
        }
        let now = now_ms();
        connection
            .execute(
                "INSERT INTO study_edges (from_node_id, to_node_id, relation, note, created_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![from_node_id, to_node_id, relation, note.trim(), now],
            )
            .into_diagnostic()
            .map_err(|err| miette!("insert relation failed: {err}"))?;
        let edge_id = connection.last_insert_rowid();
        self.bump_revision();
        Ok(StudyLinkResult {
            edge: StudyEdge {
                id: edge_id,
                from: from_node_id.to_string(),
                to: to_node_id.to_string(),
                relation: relation.to_string(),
                note: note.trim().to_string(),
            },
        })
    }

    pub fn unlink_nodes(
        &self,
        from_node_id: &str,
        to_node_id: &str,
        relation: Option<&str>,
    ) -> Result<StudyRemoveEdgeResult> {
        let connection = self.open_connection()?;
        let removed = match relation {
            Some(relation) => connection
                .execute(
                    "DELETE FROM study_edges WHERE from_node_id = ?1 AND to_node_id = ?2 AND relation = ?3",
                    params![from_node_id, to_node_id, relation],
                )
                .into_diagnostic()?,
            None => connection
                .execute(
                    "DELETE FROM study_edges WHERE from_node_id = ?1 AND to_node_id = ?2",
                    params![from_node_id, to_node_id],
                )
                .into_diagnostic()?,
        };
        if removed == 0 {
            return Err(miette!(
                "no matching relation from `{from_node_id}` to `{to_node_id}` was found"
            ));
        }
        self.bump_revision();
        Ok(StudyRemoveEdgeResult {
            removed_edges: removed,
        })
    }

    pub fn merge_nodes(
        &self,
        merged_node_id: &str,
        canonical_node_id: &str,
    ) -> Result<StudyMergeResult> {
        if merged_node_id == canonical_node_id {
            return Err(miette!("cannot merge a node into itself"));
        }
        let mut connection = self.open_connection()?;
        let transaction = connection.transaction().into_diagnostic()?;
        let merged = load_node(&transaction, merged_node_id)?;
        let mut canonical = load_node(&transaction, canonical_node_id)?;

        let merged_aliases: Vec<String> = merged
            .aliases
            .iter()
            .cloned()
            .chain(std::iter::once(merged.title.clone()))
            .collect();
        let mut all_aliases = canonical.aliases.clone();
        for alias in merged_aliases {
            if !all_aliases.iter().any(|existing| existing == &alias) {
                all_aliases.push(alias.clone());
            }
        }
        canonical.aliases = all_aliases.clone();
        for tag in &merged.tags {
            if !canonical.tags.iter().any(|existing| existing == tag) {
                canonical.tags.push(tag.clone());
            }
        }
        for source in &merged.sources {
            if !canonical.sources.iter().any(|existing| existing == source) {
                canonical.sources.push(source.clone());
            }
        }

        let mut moved_edges = 0usize;
        for (from_id, to_id, relation, note, created_at_ms) in
            load_edge_rows_for_node(&transaction, merged_node_id)?
        {
            let from = if from_id == merged_node_id {
                canonical_node_id.to_string()
            } else {
                from_id
            };
            let to = if to_id == merged_node_id {
                canonical_node_id.to_string()
            } else {
                to_id
            };
            if from == to {
                continue;
            }
            let exists: Option<i64> = transaction
                .query_row(
                    "SELECT id FROM study_edges WHERE from_node_id = ?1 AND to_node_id = ?2 AND relation = ?3",
                    params![from, to, relation],
                    |row| row.get(0),
                )
                .optional()
                .into_diagnostic()?;
            if exists.is_some() {
                continue;
            }
            transaction
                .execute(
                    "INSERT INTO study_edges (from_node_id, to_node_id, relation, note, created_at_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![from, to, relation, note, created_at_ms],
                )
                .into_diagnostic()?;
            moved_edges += 1;
        }
        transaction
            .execute(
                "DELETE FROM study_edges WHERE from_node_id = ?1 OR to_node_id = ?1",
                params![merged_node_id],
            )
            .into_diagnostic()?;

        let moved_questions = transaction
            .execute(
                "UPDATE study_questions SET node_id = ?2 WHERE node_id = ?1",
                params![merged_node_id, canonical_node_id],
            )
            .into_diagnostic()?;

        let merged_progress = load_progress(&transaction, merged_node_id)?;
        let canonical_progress = load_progress(&transaction, canonical_node_id)?;
        if merged_progress.understanding > canonical_progress.understanding {
            transaction
                .execute(
                    "UPDATE study_progress SET understanding = ?2, evidence = ?3, updated_by = ?4, updated_at_ms = ?5 WHERE node_id = ?1",
                    params![
                        canonical_node_id,
                        merged_progress.understanding,
                        merged_progress.evidence,
                        merged_progress.updated_by,
                        merged_progress.updated_at_ms,
                    ],
                )
                .into_diagnostic()?;
        }
        transaction
            .execute(
                "DELETE FROM study_progress WHERE node_id = ?1",
                params![merged_node_id],
            )
            .into_diagnostic()?;
        canonical.updated_at_ms = now_ms();
        transaction
            .execute(
                "UPDATE study_nodes
                 SET aliases = ?2, tags = ?3, sources = ?4, updated_at_ms = ?5
                 WHERE id = ?1",
                params![
                    canonical_node_id,
                    encode_string_list(&canonical.aliases),
                    encode_string_list(&canonical.tags),
                    encode_sources(&canonical.sources),
                    canonical.updated_at_ms,
                ],
            )
            .into_diagnostic()?;
        transaction
            .execute(
                "DELETE FROM study_nodes WHERE id = ?1",
                params![merged_node_id],
            )
            .into_diagnostic()?;

        if prerequisite_cycle_exists(&transaction)? {
            return Err(miette!(
                "merge would create a prerequisite cycle; adjust the duplicate's relations first"
            ));
        }

        transaction.commit().into_diagnostic()?;
        let canonical = load_node(&connection, canonical_node_id)?;
        self.bump_revision();
        Ok(StudyMergeResult {
            canonical_node_id: canonical_node_id.to_string(),
            merged_node_id: merged_node_id.to_string(),
            canonical,
            moved_edges,
            merged_aliases: all_aliases,
            moved_questions,
        })
    }

    pub fn add_questions(
        &self,
        node_id: &str,
        questions: &[NewQuestionInput],
    ) -> Result<StudyQuestionWriteResult> {
        if questions.is_empty() {
            return Err(miette!("at least one question is required"));
        }
        let mut connection = self.open_connection()?;
        let node = load_node(&connection, node_id)?;
        let transaction = connection.transaction().into_diagnostic()?;
        let mut added = Vec::new();
        for question in questions {
            let question_text = question.question.trim();
            if question_text.is_empty() {
                return Err(miette!("question text cannot be empty"));
            }
            let difficulty = question.difficulty.trim();
            let difficulty = if difficulty.is_empty() {
                "medium"
            } else {
                difficulty
            };
            if !QUESTION_DIFFICULTIES.contains(&difficulty) {
                return Err(miette!(
                    "unsupported difficulty `{difficulty}`; supported difficulties: {}",
                    QUESTION_DIFFICULTIES.join(", ")
                ));
            }
            let now = now_ms();
            let id = format!("question-{}", uuid::Uuid::new_v4());
            transaction
                .execute(
                    "INSERT INTO study_questions
                     (id, node_id, question, answer, difficulty, node_content_version, created_at_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        id,
                        node_id,
                        question_text,
                        question.answer.trim(),
                        difficulty,
                        node.content_version,
                        now
                    ],
                )
                .into_diagnostic()?;
            added.push(StudyQuestion {
                id,
                node_id: node_id.to_string(),
                question: question_text.to_string(),
                answer: question.answer.trim().to_string(),
                difficulty: difficulty.to_string(),
                node_content_version: node.content_version,
                is_stale: false,
                created_at_ms: now,
                last_outcome: None,
            });
        }
        transaction.commit().into_diagnostic()?;
        let stale_question_count = stale_question_count(&connection)?;
        self.bump_revision();
        Ok(StudyQuestionWriteResult {
            added_questions: added,
            stale_question_count,
        })
    }

    pub fn record_attempt(
        &self,
        question_id: &str,
        outcome: &str,
        note: &str,
    ) -> Result<StudyAttemptResult> {
        if !ATTEMPT_OUTCOMES.contains(&outcome) {
            return Err(miette!(
                "unsupported outcome `{outcome}`; supported outcomes: {}",
                ATTEMPT_OUTCOMES.join(", ")
            ));
        }
        let connection = self.open_connection()?;
        let node_id: String = connection
            .query_row(
                "SELECT node_id FROM study_questions WHERE id = ?1",
                params![question_id],
                |row| row.get(0),
            )
            .optional()
            .into_diagnostic()?
            .ok_or_else(|| miette!("question `{question_id}` does not exist"))?;
        let attempt_id = format!("attempt-{}", uuid::Uuid::new_v4());
        connection
            .execute(
                "INSERT INTO study_attempts (id, question_id, node_id, outcome, note, created_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![attempt_id, question_id, node_id, outcome, note.trim(), now_ms()],
            )
            .into_diagnostic()?;
        self.bump_revision();
        Ok(StudyAttemptResult {
            attempt_id,
            question_id: question_id.to_string(),
            node_id,
            outcome: outcome.to_string(),
        })
    }

    pub fn update_progress(
        &self,
        node_id: &str,
        understanding: i64,
        evidence: &str,
        origin: StudyWriteOrigin,
    ) -> Result<StudyProgressUpdate> {
        if !(0..=100).contains(&understanding) {
            return Err(miette!(
                "understanding must be a percentage between 0 and 100, got {understanding}"
            ));
        }
        let connection = self.open_connection()?;
        ensure_node_exists(&connection, node_id)?;
        let now = now_ms();
        connection
            .execute(
                "INSERT INTO study_progress (node_id, understanding, evidence, updated_by, updated_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(node_id) DO UPDATE SET
                     understanding = excluded.understanding,
                     evidence = excluded.evidence,
                     updated_by = excluded.updated_by,
                     updated_at_ms = excluded.updated_at_ms",
                params![node_id, understanding, evidence.trim(), origin.as_str(), now],
            )
            .into_diagnostic()
            .map_err(|err| miette!("update progress failed: {err}"))?;
        self.bump_revision();
        Ok(StudyProgressUpdate {
            node_id: node_id.to_string(),
            understanding,
            evidence: evidence.trim().to_string(),
            updated_by: origin.as_str().to_string(),
            updated_at_ms: now,
        })
    }

    pub fn maintenance_report(&self) -> Result<StudyMaintenance> {
        let connection = self.open_connection()?;
        let modules = self.list_modules()?;
        let nodes = load_node_summaries(&connection)?;
        load_maintenance(&connection, &modules, &nodes)
    }
}

fn migrate(connection: &Connection) -> Result<bool> {
    connection
        .execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA foreign_keys = ON;

             CREATE TABLE IF NOT EXISTS study_modules (
                 id TEXT PRIMARY KEY,
                 title TEXT NOT NULL,
                 description TEXT NOT NULL DEFAULT '',
                 created_at_ms INTEGER NOT NULL,
                 updated_at_ms INTEGER NOT NULL
             );

             CREATE TABLE IF NOT EXISTS study_nodes (
                 id TEXT PRIMARY KEY,
                 module_id TEXT NOT NULL REFERENCES study_modules(id) ON DELETE CASCADE,
                 title TEXT NOT NULL,
                 summary TEXT NOT NULL DEFAULT '',
                 body TEXT NOT NULL DEFAULT '',
                 aliases TEXT NOT NULL DEFAULT '[]',
                 tags TEXT NOT NULL DEFAULT '[]',
                 sources TEXT NOT NULL DEFAULT '[]',
                 content_version INTEGER NOT NULL DEFAULT 1,
                 created_at_ms INTEGER NOT NULL,
                 updated_at_ms INTEGER NOT NULL
             );

             CREATE INDEX IF NOT EXISTS study_nodes_module_idx ON study_nodes(module_id);

             CREATE TABLE IF NOT EXISTS study_edges (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 from_node_id TEXT NOT NULL REFERENCES study_nodes(id) ON DELETE CASCADE,
                 to_node_id TEXT NOT NULL REFERENCES study_nodes(id) ON DELETE CASCADE,
                 relation TEXT NOT NULL,
                 note TEXT NOT NULL DEFAULT '',
                 created_at_ms INTEGER NOT NULL,
                 UNIQUE(from_node_id, to_node_id, relation)
             );

             CREATE INDEX IF NOT EXISTS study_edges_from_idx ON study_edges(from_node_id);
             CREATE INDEX IF NOT EXISTS study_edges_to_idx ON study_edges(to_node_id);

             CREATE TABLE IF NOT EXISTS study_progress (
                 node_id TEXT PRIMARY KEY REFERENCES study_nodes(id) ON DELETE CASCADE,
                 understanding INTEGER NOT NULL DEFAULT 0,
                 evidence TEXT NOT NULL DEFAULT '',
                 updated_by TEXT NOT NULL DEFAULT 'code',
                 updated_at_ms INTEGER NOT NULL
             );

             CREATE TABLE IF NOT EXISTS study_questions (
                 id TEXT PRIMARY KEY,
                 node_id TEXT NOT NULL REFERENCES study_nodes(id) ON DELETE CASCADE,
                 question TEXT NOT NULL,
                 answer TEXT NOT NULL DEFAULT '',
                 difficulty TEXT NOT NULL DEFAULT 'medium',
                 node_content_version INTEGER NOT NULL,
                 created_at_ms INTEGER NOT NULL
             );

             CREATE INDEX IF NOT EXISTS study_questions_node_idx ON study_questions(node_id);

             CREATE TABLE IF NOT EXISTS study_attempts (
                 id TEXT PRIMARY KEY,
                 question_id TEXT NOT NULL REFERENCES study_questions(id) ON DELETE CASCADE,
                 node_id TEXT NOT NULL,
                 outcome TEXT NOT NULL,
                 note TEXT NOT NULL DEFAULT '',
                 created_at_ms INTEGER NOT NULL
             );

             CREATE INDEX IF NOT EXISTS study_attempts_node_idx ON study_attempts(node_id);",
        )
        .into_diagnostic()
        .map_err(|err| miette!("migrate study database failed: {err}"))?;

    match create_fts_schema(connection) {
        Ok(()) => Ok(true),
        Err(err) => {
            tracing::warn!(
                "study search falls back to LIKE matching because FTS5 is unavailable: {err:?}"
            );
            Ok(false)
        }
    }
}

fn create_fts_schema(connection: &Connection) -> Result<()> {
    connection
        .execute_batch(
            "BEGIN;
             CREATE VIRTUAL TABLE IF NOT EXISTS study_nodes_fts USING fts5(
                 node_id UNINDEXED, title, summary, body, aliases, tokenize = 'unicode61'
             );
             CREATE TRIGGER IF NOT EXISTS study_nodes_fts_insert AFTER INSERT ON study_nodes BEGIN
                 INSERT INTO study_nodes_fts (node_id, title, summary, body, aliases)
                 VALUES (new.id, new.title, new.summary, new.body, new.aliases);
             END;
             CREATE TRIGGER IF NOT EXISTS study_nodes_fts_update AFTER UPDATE ON study_nodes BEGIN
                 DELETE FROM study_nodes_fts WHERE node_id = old.id;
                 INSERT INTO study_nodes_fts (node_id, title, summary, body, aliases)
                 VALUES (new.id, new.title, new.summary, new.body, new.aliases);
             END;
             CREATE TRIGGER IF NOT EXISTS study_nodes_fts_delete AFTER DELETE ON study_nodes BEGIN
                 DELETE FROM study_nodes_fts WHERE node_id = old.id;
             END;
             COMMIT;",
        )
        .into_diagnostic()
        .map_err(|err| miette!("create study FTS schema failed: {err}"))
}

fn fts_search(connection: &Connection, query: &str, limit: usize) -> Result<Vec<String>> {
    let fts_query = build_fts_query(query);
    if fts_query.is_empty() {
        return Ok(Vec::new());
    }
    let mut statement = connection
        .prepare("SELECT node_id FROM study_nodes_fts WHERE study_nodes_fts MATCH ?1 LIMIT ?2")
        .into_diagnostic()?;
    let rows = statement
        .query_map(params![fts_query, limit as i64], |row| {
            row.get::<_, String>(0)
        })
        .into_diagnostic()?;
    let mut ids = Vec::new();
    for row in rows {
        ids.push(row.into_diagnostic()?);
    }
    Ok(ids)
}

fn build_fts_query(query: &str) -> String {
    query
        .split_whitespace()
        .map(|token| {
            let escaped = token.replace('"', "\"\"");
            format!("\"{escaped}\"*")
        })
        .collect::<Vec<_>>()
        .join(" AND ")
}

fn like_search(connection: &Connection, query: &str, limit: usize) -> Result<Vec<String>> {
    let pattern = format!("%{}%", query.to_lowercase());
    let mut statement = connection
        .prepare(
            "SELECT id FROM study_nodes
             WHERE lower(title) LIKE ?1
                OR lower(summary) LIKE ?1
                OR lower(body) LIKE ?1
                OR lower(aliases) LIKE ?1
             ORDER BY updated_at_ms DESC
             LIMIT ?2",
        )
        .into_diagnostic()?;
    let rows = statement
        .query_map(params![pattern, limit as i64], |row| {
            row.get::<_, String>(0)
        })
        .into_diagnostic()?;
    let mut ids = Vec::new();
    for row in rows {
        ids.push(row.into_diagnostic()?);
    }
    Ok(ids)
}

fn load_node_summaries(connection: &Connection) -> Result<Vec<StudyNodeSummary>> {
    let mut statement = connection
        .prepare(
            "SELECT n.id, n.module_id, n.title, n.summary, n.aliases, n.tags, n.content_version,
                    COALESCE(p.understanding, 0), COALESCE(p.evidence, ''), COALESCE(p.updated_by, 'code'),
                    COALESCE(p.updated_at_ms, 0),
                    (SELECT COUNT(*) FROM study_questions q WHERE q.node_id = n.id),
                    (SELECT COUNT(*) FROM study_questions q
                      WHERE q.node_id = n.id AND q.node_content_version != n.content_version)
             FROM study_nodes n
             LEFT JOIN study_progress p ON p.node_id = n.id
             ORDER BY n.module_id ASC, n.created_at_ms ASC, n.id ASC",
        )
        .into_diagnostic()?;
    let rows = statement
        .query_map([], |row| {
            Ok(StudyNodeSummary {
                id: row.get(0)?,
                module_id: row.get(1)?,
                title: row.get(2)?,
                summary: row.get(3)?,
                aliases: decode_string_list(&row.get::<_, String>(4)?),
                tags: decode_string_list(&row.get::<_, String>(5)?),
                content_version: row.get(6)?,
                progress: StudyProgress {
                    understanding: row.get(7)?,
                    evidence: row.get(8)?,
                    updated_by: row.get(9)?,
                    updated_at_ms: row.get(10)?,
                },
                question_count: row.get::<_, i64>(11)? as usize,
                stale_question_count: row.get::<_, i64>(12)? as usize,
            })
        })
        .into_diagnostic()?;
    let mut nodes = Vec::new();
    for row in rows {
        nodes.push(row.into_diagnostic()?);
    }
    Ok(nodes)
}

fn load_edges(connection: &Connection) -> Result<Vec<StudyEdge>> {
    let mut statement = connection
        .prepare(
            "SELECT id, from_node_id, to_node_id, relation, note
             FROM study_edges ORDER BY id ASC",
        )
        .into_diagnostic()?;
    let rows = statement
        .query_map([], |row| {
            Ok(StudyEdge {
                id: row.get(0)?,
                from: row.get(1)?,
                to: row.get(2)?,
                relation: row.get(3)?,
                note: row.get(4)?,
            })
        })
        .into_diagnostic()?;
    let mut edges = Vec::new();
    for row in rows {
        edges.push(row.into_diagnostic()?);
    }
    Ok(edges)
}

type EdgeRow = (String, String, String, String, i64);

fn load_edge_rows_for_node(connection: &Connection, node_id: &str) -> Result<Vec<EdgeRow>> {
    let mut statement = connection
        .prepare(
            "SELECT from_node_id, to_node_id, relation, note, created_at_ms
             FROM study_edges WHERE from_node_id = ?1 OR to_node_id = ?1",
        )
        .into_diagnostic()?;
    let rows = statement
        .query_map(params![node_id], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .into_diagnostic()?;
    let mut edges = Vec::new();
    for row in rows {
        edges.push(row.into_diagnostic()?);
    }
    Ok(edges)
}

fn load_node(connection: &Connection, node_id: &str) -> Result<StudyNode> {
    connection
        .query_row(
            "SELECT id, module_id, title, summary, body, aliases, tags, sources,
                    content_version, created_at_ms, updated_at_ms
             FROM study_nodes WHERE id = ?1",
            params![node_id],
            |row| {
                Ok(StudyNode {
                    id: row.get(0)?,
                    module_id: row.get(1)?,
                    title: row.get(2)?,
                    summary: row.get(3)?,
                    body: row.get(4)?,
                    aliases: decode_string_list(&row.get::<_, String>(5)?),
                    tags: decode_string_list(&row.get::<_, String>(6)?),
                    sources: decode_sources(&row.get::<_, String>(7)?),
                    content_version: row.get(8)?,
                    created_at_ms: row.get(9)?,
                    updated_at_ms: row.get(10)?,
                })
            },
        )
        .optional()
        .into_diagnostic()?
        .ok_or_else(|| miette!("study node `{node_id}` does not exist"))
}

fn load_progress(connection: &Connection, node_id: &str) -> Result<StudyProgress> {
    let progress = connection
        .query_row(
            "SELECT understanding, evidence, updated_by, updated_at_ms FROM study_progress WHERE node_id = ?1",
            params![node_id],
            |row| {
                Ok(StudyProgress {
                    understanding: row.get(0)?,
                    evidence: row.get(1)?,
                    updated_by: row.get(2)?,
                    updated_at_ms: row.get(3)?,
                })
            },
        )
        .optional()
        .into_diagnostic()?;
    Ok(progress.unwrap_or_default())
}

fn load_questions(
    connection: &Connection,
    node_id: &str,
    content_version: i64,
) -> Result<Vec<StudyQuestion>> {
    let mut statement = connection
        .prepare(
            "SELECT q.id, q.node_id, q.question, q.answer, q.difficulty, q.node_content_version, q.created_at_ms,
                    (SELECT a.outcome FROM study_attempts a
                      WHERE a.question_id = q.id ORDER BY a.created_at_ms DESC LIMIT 1)
             FROM study_questions q
             WHERE q.node_id = ?1
             ORDER BY q.created_at_ms ASC, q.id ASC",
        )
        .into_diagnostic()?;
    let rows = statement
        .query_map(params![node_id], |row| {
            let question_version: i64 = row.get(5)?;
            Ok(StudyQuestion {
                id: row.get(0)?,
                node_id: row.get(1)?,
                question: row.get(2)?,
                answer: row.get(3)?,
                difficulty: row.get(4)?,
                node_content_version: question_version,
                is_stale: question_version != content_version,
                created_at_ms: row.get(6)?,
                last_outcome: row.get(7)?,
            })
        })
        .into_diagnostic()?;
    let mut questions = Vec::new();
    for row in rows {
        questions.push(row.into_diagnostic()?);
    }
    Ok(questions)
}

fn load_neighbors(connection: &Connection, node: &StudyNode) -> Result<Vec<StudyNeighbor>> {
    let summaries = load_node_summaries(connection)?;
    let mut neighbors = Vec::new();
    for edge in load_edges(connection)? {
        let (neighbor_id, direction) = if edge.from == node.id {
            (edge.to.clone(), "out")
        } else if edge.to == node.id {
            (edge.from.clone(), "in")
        } else {
            continue;
        };
        let Some(neighbor) = summaries.iter().find(|item| item.id == neighbor_id) else {
            continue;
        };
        neighbors.push(StudyNeighbor {
            node: neighbor.clone(),
            relation: edge.relation,
            direction: direction.to_string(),
            note: edge.note,
        });
    }
    Ok(neighbors)
}

fn load_maintenance(
    connection: &Connection,
    modules: &[StudyModuleSummary],
    nodes: &[StudyNodeSummary],
) -> Result<StudyMaintenance> {
    let linked_ids = {
        let mut statement = connection
            .prepare("SELECT DISTINCT from_node_id FROM study_edges UNION SELECT DISTINCT to_node_id FROM study_edges")
            .into_diagnostic()?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .into_diagnostic()?;
        let mut ids = Vec::new();
        for row in rows {
            ids.push(row.into_diagnostic()?);
        }
        ids
    };
    let orphan_node_ids: Vec<String> = nodes
        .iter()
        .filter(|node| !linked_ids.iter().any(|id| id == &node.id))
        .map(|node| node.id.clone())
        .collect();

    let mut empty_module_ids = Vec::new();
    for module in modules {
        if module.node_count == 0 {
            empty_module_ids.push(module.module.id.clone());
        }
    }

    let mut identity_groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for node in nodes {
        for identity in std::iter::once(node.title.clone()).chain(node.aliases.iter().cloned()) {
            let normalized = normalize_identity(&identity);
            if normalized.is_empty() {
                continue;
            }
            let group = identity_groups.entry(normalized).or_default();
            if !group.iter().any(|id| id == &node.id) {
                group.push(node.id.clone());
            }
        }
    }
    let mut unique_groups: BTreeMap<Vec<String>, ()> = BTreeMap::new();
    for (_identity, mut group) in identity_groups {
        if group.len() < 2 {
            continue;
        }
        group.sort();
        group.dedup();
        unique_groups.insert(group, ());
    }
    let duplicate_candidate_groups: Vec<Vec<String>> = unique_groups.into_keys().collect();

    Ok(StudyMaintenance {
        orphan_node_ids,
        empty_module_ids,
        duplicate_candidate_groups,
        unlinked_node_ids: Vec::new(),
        stale_question_count: stale_question_count(connection)?,
    })
}

fn stale_question_count(connection: &Connection) -> Result<usize> {
    let count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM study_questions q
             JOIN study_nodes n ON n.id = q.node_id
             WHERE q.node_content_version != n.content_version",
            [],
            |row| row.get(0),
        )
        .into_diagnostic()?;
    Ok(count as usize)
}

fn count_rows(connection: &Connection, table: &str) -> Result<usize> {
    let count: i64 = connection
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .into_diagnostic()?;
    Ok(count as usize)
}

fn prerequisite_path_exists(connection: &Connection, from: &str, target: &str) -> Result<bool> {
    let mut visited = std::collections::HashSet::new();
    let mut queue = std::collections::VecDeque::new();
    queue.push_back(from.to_string());
    while let Some(current) = queue.pop_front() {
        if current == target {
            return Ok(true);
        }
        if !visited.insert(current.clone()) {
            continue;
        }
        let mut statement = connection
            .prepare("SELECT to_node_id FROM study_edges WHERE from_node_id = ?1 AND relation = 'prerequisite'")
            .into_diagnostic()?;
        let rows = statement
            .query_map(params![current], |row| row.get::<_, String>(0))
            .into_diagnostic()?;
        for row in rows {
            queue.push_back(row.into_diagnostic()?);
        }
    }
    Ok(false)
}

fn prerequisite_cycle_exists(connection: &Connection) -> Result<bool> {
    let mut statement = connection
        .prepare("SELECT from_node_id FROM study_edges WHERE relation = 'prerequisite'")
        .into_diagnostic()?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .into_diagnostic()?;
    for row in rows {
        let node = row.into_diagnostic()?;
        if prerequisite_path_exists(connection, &node, &node)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn ensure_node_exists(connection: &Connection, node_id: &str) -> Result<()> {
    let exists: Option<i64> = connection
        .query_row(
            "SELECT 1 FROM study_nodes WHERE id = ?1",
            params![node_id],
            |row| row.get(0),
        )
        .optional()
        .into_diagnostic()?;
    if exists.is_none() {
        return Err(miette!("study node `{node_id}` does not exist"));
    }
    Ok(())
}

fn apply_understanding(summary: &mut StudyModuleSummary, understanding: i64) {
    summary.node_count += 1;
    if understanding >= STUDY_MASTERED_THRESHOLD {
        summary.mastered_count += 1;
    } else if understanding > 0 {
        summary.in_progress_count += 1;
    } else {
        summary.unseen_count += 1;
    }
}

fn set_content_field(field: &mut String, incoming: Option<&str>, changed: &mut bool) {
    if let Some(incoming) = incoming {
        let incoming = incoming.trim();
        if incoming != field {
            *field = incoming.to_string();
            *changed = true;
        }
    }
}

fn normalize_string_list(values: &[String]) -> Vec<String> {
    let mut normalized = Vec::new();
    for value in values {
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        if !normalized.iter().any(|existing| existing == value) {
            normalized.push(value.to_string());
        }
    }
    normalized
}

fn normalize_identity(value: &str) -> String {
    value
        .trim()
        .to_lowercase()
        .chars()
        .filter(|ch| {
            !ch.is_whitespace() && !matches!(ch, '-' | '_' | '·' | '/' | '(' | ')' | '（' | '）')
        })
        .collect()
}

fn encode_string_list(values: &[String]) -> String {
    serde_json::to_string(values).unwrap_or_else(|_| "[]".to_string())
}

fn decode_string_list(value: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(value).unwrap_or_default()
}

fn encode_sources(sources: &[StudySource]) -> String {
    serde_json::to_string(sources).unwrap_or_else(|_| "[]".to_string())
}

fn decode_sources(value: &str) -> Vec<StudySource> {
    serde_json::from_str::<Vec<StudySource>>(value).unwrap_or_default()
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, StudyStore) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = StudyStore::open_at(dir.path().join("graph.sqlite3")).expect("open store");
        (dir, store)
    }

    fn create_module(store: &StudyStore, title: &str) -> StudyModule {
        store
            .create_module(title, "", StudyWriteOrigin::Agent)
            .expect("create module")
    }

    fn source() -> StudySource {
        StudySource {
            title: "textbook".to_string(),
            url: "https://example.com/book".to_string(),
        }
    }

    fn create_node(store: &StudyStore, module_id: &str, title: &str) -> StudyNode {
        store
            .create_node(
                &CreateNodeInput {
                    module_id: module_id.to_string(),
                    title: title.to_string(),
                    summary: format!("{title} summary"),
                    body: format!("{title} body"),
                    aliases: Vec::new(),
                    tags: Vec::new(),
                    sources: vec![source()],
                },
                StudyWriteOrigin::Agent,
            )
            .expect("create node")
            .node
    }

    #[test]
    fn new_nodes_require_sources() {
        let (_dir, store) = store();
        let module = create_module(&store, "Algebra");
        let err = store
            .create_node(
                &CreateNodeInput {
                    module_id: module.id.clone(),
                    title: "Group".to_string(),
                    ..CreateNodeInput::default()
                },
                StudyWriteOrigin::Agent,
            )
            .expect_err("sources required");
        assert!(
            err.to_string().contains("source"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn new_nodes_require_existing_module() {
        let (_dir, store) = store();
        let err = store
            .create_node(
                &CreateNodeInput {
                    module_id: "module-missing".to_string(),
                    title: "Group".to_string(),
                    sources: vec![source()],
                    ..CreateNodeInput::default()
                },
                StudyWriteOrigin::Agent,
            )
            .expect_err("module must exist");
        assert!(
            err.to_string().contains("does not exist"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn prerequisite_links_reject_cycles() {
        let (_dir, store) = store();
        let module = create_module(&store, "Algebra");
        let a = create_node(&store, &module.id, "A");
        let b = create_node(&store, &module.id, "B");
        store
            .link_nodes(&a.id, &b.id, "prerequisite", "a before b")
            .expect("A -> B prerequisite");
        let err = store
            .link_nodes(&b.id, &a.id, "prerequisite", "b before a")
            .expect_err("cycle must be rejected");
        assert!(err.to_string().contains("cycle"), "unexpected error: {err}");
    }

    #[test]
    fn duplicate_relations_are_rejected() {
        let (_dir, store) = store();
        let module = create_module(&store, "Algebra");
        let a = create_node(&store, &module.id, "A");
        let b = create_node(&store, &module.id, "B");
        store.link_nodes(&a.id, &b.id, "related", "").expect("link");
        let err = store
            .link_nodes(&a.id, &b.id, "related", "")
            .expect_err("duplicate must be rejected");
        assert!(err.to_string().contains("already exists"));
    }

    #[test]
    fn content_updates_bump_versions_and_stale_questions() {
        let (_dir, store) = store();
        let module = create_module(&store, "Algebra");
        let node = create_node(&store, &module.id, "Group");
        store
            .add_questions(
                &node.id,
                &[NewQuestionInput {
                    question: "What is a group?".to_string(),
                    answer: "A set with an operation.".to_string(),
                    difficulty: "easy".to_string(),
                }],
            )
            .expect("add question");

        let detail = store.node_detail(&node.id).expect("detail");
        assert_eq!(detail.questions.len(), 1);
        assert!(!detail.questions[0].is_stale);

        store
            .update_node(
                &node.id,
                &UpdateNodeInput {
                    body: Some("Refined body".to_string()),
                    ..UpdateNodeInput::default()
                },
            )
            .expect("update body");

        let detail = store.node_detail(&node.id).expect("detail");
        assert_eq!(detail.node.content_version, 2);
        assert!(detail.questions[0].is_stale);
        assert_eq!(detail.questions[0].node_content_version, 1);
    }

    #[test]
    fn merge_moves_edges_aliases_and_questions() {
        let (_dir, store) = store();
        let module = create_module(&store, "Algebra");
        let canonical = create_node(&store, &module.id, "Group");
        let duplicate = create_node(&store, &module.id, "Groups");
        let other = create_node(&store, &module.id, "Ring");
        store
            .link_nodes(&duplicate.id, &other.id, "related", "")
            .expect("link duplicate");
        store
            .add_questions(
                &duplicate.id,
                &[NewQuestionInput {
                    question: "Is a group a ring?".to_string(),
                    answer: String::new(),
                    difficulty: "medium".to_string(),
                }],
            )
            .expect("question");

        let result = store
            .merge_nodes(&duplicate.id, &canonical.id)
            .expect("merge");
        assert_eq!(result.moved_edges, 1);
        assert_eq!(result.moved_questions, 1);
        assert!(
            result
                .canonical
                .aliases
                .iter()
                .any(|alias| alias == "Groups")
        );
        assert!(store.node_detail(&duplicate.id).is_err());
        let canonical_detail = store.node_detail(&canonical.id).expect("canonical detail");
        assert_eq!(canonical_detail.questions.len(), 1);
        assert!(
            canonical_detail
                .neighbors
                .iter()
                .any(|neighbor| neighbor.node.id == other.id)
        );
    }

    #[test]
    fn merge_rejects_cycles() {
        let (_dir, store) = store();
        let module = create_module(&store, "Algebra");
        let a = create_node(&store, &module.id, "A");
        let b = create_node(&store, &module.id, "B");
        let a2 = create_node(&store, &module.id, "A duplicate");
        store
            .link_nodes(&a2.id, &b.id, "prerequisite", "")
            .expect("a2 -> b");
        store
            .link_nodes(&b.id, &a.id, "prerequisite", "")
            .expect("b -> a");
        let err = store
            .merge_nodes(&a2.id, &a.id)
            .expect_err("merge would create cycle");
        assert!(err.to_string().contains("cycle"), "unexpected error: {err}");
        assert!(store.node_detail(&a2.id).is_ok());
    }

    #[test]
    fn progress_writes_record_origin_and_evidence() {
        let (_dir, store) = store();
        let module = create_module(&store, "Algebra");
        let node = create_node(&store, &module.id, "Group");
        let update = store
            .update_progress(&node.id, 100, "user_declared", StudyWriteOrigin::User)
            .expect("update progress");
        assert_eq!(update.updated_by, "user");
        assert_eq!(update.understanding, 100);

        let snapshot = store.graph_snapshot().expect("snapshot");
        assert_eq!(snapshot.stats.mastered_count, 1);
        assert_eq!(snapshot.stats.unseen_count, 0);
        assert_eq!(snapshot.stats.average_understanding, 100);

        let err = store
            .update_progress(&node.id, 140, "", StudyWriteOrigin::Agent)
            .expect_err("invalid percentage");
        assert!(
            err.to_string()
                .contains("understanding must be a percentage between 0 and 100"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn search_matches_titles_aliases_and_content() {
        let (_dir, store) = store();
        let module = create_module(&store, "Algebra");
        let _node = store
            .create_node(
                &CreateNodeInput {
                    module_id: module.id.clone(),
                    title: "Group".to_string(),
                    summary: "algebraic structure".to_string(),
                    body: "A group is a set with an associative operation.".to_string(),
                    aliases: vec!["群".to_string()],
                    tags: Vec::new(),
                    sources: vec![source()],
                },
                StudyWriteOrigin::Agent,
            )
            .expect("create node");

        let by_title = store.search_nodes("group", None, 10).expect("search");
        assert_eq!(by_title.len(), 1);
        let by_alias = store.search_nodes("群", None, 10).expect("search");
        assert_eq!(by_alias.len(), 1);
        let by_body = store.search_nodes("associative", None, 10).expect("search");
        assert_eq!(by_body.len(), 1);
    }

    #[test]
    fn maintenance_reports_orphans_and_duplicate_candidates() {
        let (_dir, store) = store();
        let module = create_module(&store, "Algebra");
        let isolated = create_node(&store, &module.id, "Isolated");
        let _group = create_node(&store, &module.id, "Group");
        let _groups = store
            .create_node(
                &CreateNodeInput {
                    module_id: module.id.clone(),
                    title: "Groups".to_string(),
                    aliases: vec!["Group".to_string()],
                    sources: vec![source()],
                    ..CreateNodeInput::default()
                },
                StudyWriteOrigin::Agent,
            )
            .expect("duplicate candidate")
            .node;

        let report = store.maintenance_report().expect("maintenance");
        assert!(report.orphan_node_ids.contains(&isolated.id));
        assert!(
            report
                .duplicate_candidate_groups
                .iter()
                .any(|group| group.len() > 1),
            "expected duplicate candidates: {report:?}"
        );
    }

    #[test]
    fn mutations_bump_revision() {
        let (_dir, store) = store();
        let start = store.revision();
        let module = create_module(&store, "Algebra");
        let after_module = store.revision();
        assert!(after_module > start);

        let _node = create_node(&store, &module.id, "Group");
        let after_node = store.revision();
        assert!(after_node > after_module);

        let snapshot = store.graph_snapshot().expect("snapshot");
        assert_eq!(snapshot.revision, store.revision());
    }

    #[test]
    fn snapshot_includes_modules_nodes_edges_and_stats() {
        let (_dir, store) = store();
        let module = create_module(&store, "Algebra");
        let a = create_node(&store, &module.id, "A");
        let b = create_node(&store, &module.id, "B");
        store
            .link_nodes(&a.id, &b.id, "prerequisite", "")
            .expect("link");

        let snapshot = store.graph_snapshot().expect("snapshot");
        assert_eq!(snapshot.stats.module_count, 1);
        assert_eq!(snapshot.stats.node_count, 2);
        assert_eq!(snapshot.stats.edge_count, 1);
        assert_eq!(snapshot.modules[0].node_count, 2);
        assert_eq!(snapshot.nodes.len(), 2);
        assert_eq!(snapshot.edges[0].relation, "prerequisite");
    }
}
