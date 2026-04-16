use std::{
    cmp::Ordering,
    collections::{BTreeMap, HashSet},
    path::PathBuf,
};

use chrono::Utc;
use miette::{Result, miette};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::daat_locus_paths::daat_locus_paths;

const SKILL_REGISTRY_FILE_NAME: &str = "skills_registry.json";
const MAX_SUMMARY_ITEMS: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SkillStatus {
    Draft,
    Active,
    Archived,
    Deprecated,
}

impl Default for SkillStatus {
    fn default() -> Self {
        Self::Draft
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct SkillQualityMetrics {
    #[serde(default)]
    pub total_runs: u64,
    #[serde(default)]
    pub success_runs: u64,
    #[serde(default)]
    pub failure_runs: u64,
    #[serde(default)]
    pub avg_steps: f64,
    #[serde(default)]
    pub regression_count: u64,
}

impl SkillQualityMetrics {
    pub fn success_rate(&self) -> Option<f64> {
        if self.total_runs == 0 {
            None
        } else {
            Some(self.success_runs as f64 / self.total_runs as f64)
        }
    }

    fn apply_outcome(&mut self, outcome: &SkillOutcomeLog) {
        self.total_runs = self.total_runs.saturating_add(1);
        if outcome.success {
            self.success_runs = self.success_runs.saturating_add(1);
        } else {
            self.failure_runs = self.failure_runs.saturating_add(1);
        }
        if outcome.regression {
            self.regression_count = self.regression_count.saturating_add(1);
        }

        if let Some(steps) = outcome.steps_executed {
            let previous_weight = (self.total_runs.saturating_sub(1)) as f64;
            let next = steps as f64;
            self.avg_steps = if previous_weight <= 0.0 {
                next
            } else {
                ((self.avg_steps * previous_weight) + next) / (previous_weight + 1.0)
            };
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct SkillMergeLineage {
    #[serde(default)]
    pub from_skill_ids: Vec<String>,
    pub reason: Option<String>,
    #[serde(default)]
    pub at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SkillRecord {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub status: SkillStatus,
    #[serde(default)]
    pub version: u64,
    #[serde(default)]
    pub trigger_conditions: Vec<String>,
    #[serde(default)]
    pub preconditions: Vec<String>,
    #[serde(default)]
    pub workflow_steps: Vec<String>,
    #[serde(default)]
    pub done_criteria: Vec<String>,
    #[serde(default)]
    pub failure_recovery: Vec<String>,
    #[serde(default)]
    pub quality_metrics: SkillQualityMetrics,
    #[serde(default)]
    pub merge_lineage: Vec<SkillMergeLineage>,
    #[serde(default)]
    pub created_at_ms: i64,
    #[serde(default)]
    pub updated_at_ms: i64,
}

impl SkillRecord {
    fn normalize(mut self) -> Result<Self> {
        self.id = normalize_identifier(&self.id);
        self.name = self.name.trim().to_string();
        self.trigger_conditions = normalize_string_list(self.trigger_conditions);
        self.preconditions = normalize_string_list(self.preconditions);
        self.workflow_steps = normalize_string_list(self.workflow_steps);
        self.done_criteria = normalize_string_list(self.done_criteria);
        self.failure_recovery = normalize_string_list(self.failure_recovery);

        if self.id.is_empty() {
            return Err(miette!("skill.id cannot be empty"));
        }
        if self.name.is_empty() {
            return Err(miette!("skill.name cannot be empty"));
        }
        if self.workflow_steps.is_empty() {
            return Err(miette!("skill.workflow_steps cannot be empty"));
        }
        if self.version == 0 {
            self.version = 1;
        }
        if self.created_at_ms == 0 {
            self.created_at_ms = Utc::now().timestamp_millis();
        }
        if self.updated_at_ms == 0 {
            self.updated_at_ms = self.created_at_ms;
        }
        Ok(self)
    }

    pub fn compact_summary(&self) -> SkillSummary {
        SkillSummary {
            id: self.id.clone(),
            name: self.name.clone(),
            status: self.status,
            version: self.version,
            success_rate: self.quality_metrics.success_rate(),
            avg_steps: self.quality_metrics.avg_steps,
            updated_at_ms: self.updated_at_ms,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SkillSummary {
    pub id: String,
    pub name: String,
    pub status: SkillStatus,
    pub version: u64,
    pub success_rate: Option<f64>,
    pub avg_steps: f64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SkillMatch {
    pub score: f64,
    pub summary: SkillSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct NewSkillRecord {
    pub name: String,
    #[serde(default)]
    pub trigger_conditions: Vec<String>,
    #[serde(default)]
    pub preconditions: Vec<String>,
    #[serde(default)]
    pub workflow_steps: Vec<String>,
    #[serde(default)]
    pub done_criteria: Vec<String>,
    #[serde(default)]
    pub failure_recovery: Vec<String>,
}

impl NewSkillRecord {
    pub fn into_skill_record(self, existing_ids: &HashSet<String>) -> SkillRecord {
        let now = Utc::now().timestamp_millis();
        let mut base = slugify_skill_name(&self.name);
        if base.is_empty() {
            base = "skill".to_string();
        }
        let mut id = base.clone();
        let mut suffix = 2_u64;
        while existing_ids.contains(&id) {
            id = format!("{base}-{suffix}");
            suffix = suffix.saturating_add(1);
        }

        SkillRecord {
            id,
            name: self.name,
            status: SkillStatus::Draft,
            version: 1,
            trigger_conditions: self.trigger_conditions,
            preconditions: self.preconditions,
            workflow_steps: self.workflow_steps,
            done_criteria: self.done_criteria,
            failure_recovery: self.failure_recovery,
            quality_metrics: SkillQualityMetrics::default(),
            merge_lineage: Vec::new(),
            created_at_ms: now,
            updated_at_ms: now,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SkillOutcomeLog {
    pub success: bool,
    pub steps_executed: Option<u32>,
    #[serde(default)]
    pub regression: bool,
    pub summary: Option<String>,
    pub failure_type: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct SkillGovernanceReport {
    #[serde(default)]
    pub duplicate_pairs: Vec<(String, String)>,
    #[serde(default)]
    pub low_quality_skill_ids: Vec<String>,
    #[serde(default)]
    pub cold_skill_ids: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SkillRegistryPersisted {
    #[serde(default)]
    skills: Vec<SkillRecord>,
}

pub struct SkillRegistry {
    storage_path: PathBuf,
    skills: BTreeMap<String, SkillRecord>,
    dirty: bool,
}

impl SkillRegistry {
    pub async fn new() -> Self {
        let storage_path = daat_locus_paths()
            .await
            .state_file(SKILL_REGISTRY_FILE_NAME);
        let mut registry = Self {
            storage_path,
            skills: BTreeMap::new(),
            dirty: false,
        };
        registry.load_from_disk().await;
        registry
    }

    pub fn get(&self, skill_id: &str) -> Option<&SkillRecord> {
        self.skills.get(skill_id)
    }

    pub fn list(&self) -> Vec<SkillRecord> {
        self.skills.values().cloned().collect()
    }

    pub fn summaries(&self, limit: usize) -> Vec<SkillSummary> {
        let mut items = self
            .skills
            .values()
            .map(SkillRecord::compact_summary)
            .collect::<Vec<_>>();
        items.sort_by(|left, right| right.updated_at_ms.cmp(&left.updated_at_ms));
        items.truncate(limit.min(MAX_SUMMARY_ITEMS));
        items
    }

    pub fn query(&self, query: &str, limit: usize) -> Vec<SkillMatch> {
        let tokens = tokenize(query);
        if tokens.is_empty() {
            return self
                .summaries(limit)
                .into_iter()
                .map(|summary| SkillMatch {
                    score: 0.0,
                    summary,
                })
                .collect();
        }

        let mut matches = self
            .skills
            .values()
            .filter_map(|record| {
                let score = score_skill(record, &tokens);
                (score > 0.0).then(|| SkillMatch {
                    score,
                    summary: record.compact_summary(),
                })
            })
            .collect::<Vec<_>>();

        matches.sort_by(|left, right| {
            right
                .score
                .partial_cmp(&left.score)
                .unwrap_or(Ordering::Equal)
                .then_with(|| right.summary.updated_at_ms.cmp(&left.summary.updated_at_ms))
        });
        matches.truncate(limit.min(MAX_SUMMARY_ITEMS));
        matches
    }

    pub fn create_skill(&mut self, draft: NewSkillRecord) -> Result<SkillRecord> {
        if draft.name.trim().is_empty() {
            return Err(miette!("create_skill requires non-empty name"));
        }
        if draft.workflow_steps.is_empty() {
            return Err(miette!("create_skill requires at least one workflow step"));
        }

        let existing_ids = self.skills.keys().cloned().collect::<HashSet<_>>();
        let record = draft.into_skill_record(&existing_ids).normalize()?;
        self.skills.insert(record.id.clone(), record.clone());
        self.dirty = true;
        Ok(record)
    }

    pub fn activate_skill(&mut self, skill_id: &str) -> Result<SkillRecord> {
        let record = self
            .skills
            .get_mut(skill_id)
            .ok_or_else(|| miette!("unknown skill_id `{skill_id}`"))?;
        let now = Utc::now().timestamp_millis();
        if !matches!(record.status, SkillStatus::Active) {
            record.status = SkillStatus::Active;
            record.version = record.version.saturating_add(1);
        }
        record.updated_at_ms = now;
        self.dirty = true;
        Ok(record.clone())
    }

    pub fn log_outcome(&mut self, skill_id: &str, outcome: SkillOutcomeLog) -> Result<SkillRecord> {
        let record = self
            .skills
            .get_mut(skill_id)
            .ok_or_else(|| miette!("unknown skill_id `{skill_id}`"))?;

        record.quality_metrics.apply_outcome(&outcome);
        record.updated_at_ms = Utc::now().timestamp_millis();
        record.version = record.version.saturating_add(1);
        if outcome.success && !matches!(record.status, SkillStatus::Active) {
            record.status = SkillStatus::Active;
        }
        self.dirty = true;
        Ok(record.clone())
    }

    pub fn apply_sleep_patch(
        &mut self,
        skill_id: &str,
        workflow_step_additions: Vec<String>,
        failure_recovery_additions: Vec<String>,
    ) -> Result<SkillRecord> {
        let record = self
            .skills
            .get_mut(skill_id)
            .ok_or_else(|| miette!("unknown skill_id `{skill_id}`"))?;

        let before_workflow_len = record.workflow_steps.len();
        let before_recovery_len = record.failure_recovery.len();

        extend_unique(&mut record.workflow_steps, normalize_string_list(workflow_step_additions));
        extend_unique(
            &mut record.failure_recovery,
            normalize_string_list(failure_recovery_additions),
        );

        if record.workflow_steps.is_empty() {
            return Err(miette!(
                "apply_sleep_patch cannot produce empty workflow_steps for `{skill_id}`"
            ));
        }

        let changed = record.workflow_steps.len() != before_workflow_len
            || record.failure_recovery.len() != before_recovery_len;
        if changed {
            record.version = record.version.saturating_add(1);
            record.updated_at_ms = Utc::now().timestamp_millis();
            self.dirty = true;
        }

        Ok(record.clone())
    }

    pub fn deprecate_skill(&mut self, skill_id: &str, reason: Option<String>) -> Result<SkillRecord> {
        let record = self
            .skills
            .get_mut(skill_id)
            .ok_or_else(|| miette!("unknown skill_id `{skill_id}`"))?;

        let normalized_reason = reason.and_then(|value| {
            let trimmed = value.trim().to_string();
            (!trimmed.is_empty()).then_some(trimmed)
        });

        let changed = !matches!(record.status, SkillStatus::Deprecated);
        record.status = SkillStatus::Deprecated;
        record.updated_at_ms = Utc::now().timestamp_millis();
        if changed {
            record.version = record.version.saturating_add(1);
        }
        if normalized_reason.is_some() {
            record.merge_lineage.push(SkillMergeLineage {
                from_skill_ids: Vec::new(),
                reason: normalized_reason,
                at_ms: record.updated_at_ms,
            });
        }
        self.dirty = true;
        Ok(record.clone())
    }

    pub fn merge_skills(
        &mut self,
        target_skill_id: &str,
        source_skill_ids: &[String],
        reason: Option<String>,
    ) -> Result<SkillRecord> {
        let target_exists = self.skills.contains_key(target_skill_id);
        if !target_exists {
            return Err(miette!("unknown target skill_id `{target_skill_id}`"));
        }

        let source_ids = source_skill_ids
            .iter()
            .map(|item| normalize_identifier(item))
            .filter(|item| !item.is_empty() && item != target_skill_id)
            .collect::<Vec<_>>();
        if source_ids.is_empty() {
            return Err(miette!("merge_skills requires at least one source skill"));
        }

        let sources = source_ids
            .iter()
            .map(|source_id| {
                self.skills
                    .get(source_id)
                    .cloned()
                    .ok_or_else(|| miette!("unknown source skill_id `{source_id}`"))
            })
            .collect::<Result<Vec<_>>>()?;

        {
            let target = self
                .skills
                .get_mut(target_skill_id)
                .ok_or_else(|| miette!("unknown target skill_id `{target_skill_id}`"))?;

            for source in &sources {
                extend_unique(
                    &mut target.trigger_conditions,
                    normalize_string_list(source.trigger_conditions.clone()),
                );
                extend_unique(
                    &mut target.preconditions,
                    normalize_string_list(source.preconditions.clone()),
                );
                extend_unique(
                    &mut target.workflow_steps,
                    normalize_string_list(source.workflow_steps.clone()),
                );
                extend_unique(
                    &mut target.done_criteria,
                    normalize_string_list(source.done_criteria.clone()),
                );
                extend_unique(
                    &mut target.failure_recovery,
                    normalize_string_list(source.failure_recovery.clone()),
                );
            }

            target.version = target.version.saturating_add(1);
            target.updated_at_ms = Utc::now().timestamp_millis();
            target.merge_lineage.push(SkillMergeLineage {
                from_skill_ids: source_ids.clone(),
                reason: reason.and_then(|item| {
                    let trimmed = item.trim().to_string();
                    (!trimmed.is_empty()).then_some(trimmed)
                }),
                at_ms: target.updated_at_ms,
            });
        }

        for source_id in &source_ids {
            if let Some(source) = self.skills.get_mut(source_id) {
                source.status = SkillStatus::Archived;
                source.version = source.version.saturating_add(1);
                source.updated_at_ms = Utc::now().timestamp_millis();
            }
        }

        self.dirty = true;
        self
            .skills
            .get(target_skill_id)
            .cloned()
            .ok_or_else(|| miette!("unknown target skill_id `{target_skill_id}`"))
    }

    pub fn governance_report(&self) -> SkillGovernanceReport {
        let records = self.skills.values().collect::<Vec<_>>();

        let mut duplicate_pairs = Vec::new();
        for left in 0..records.len() {
            for right in (left + 1)..records.len() {
                let left_record = records[left];
                let right_record = records[right];
                if name_similarity(&left_record.name, &right_record.name) >= 0.8 {
                    duplicate_pairs.push((left_record.id.clone(), right_record.id.clone()));
                }
            }
        }

        let low_quality_skill_ids = records
            .iter()
            .filter(|record| record.quality_metrics.total_runs >= 3)
            .filter_map(|record| {
                record
                    .quality_metrics
                    .success_rate()
                    .filter(|rate| *rate < 0.4)
                    .map(|_| record.id.clone())
            })
            .collect::<Vec<_>>();

        let cold_skill_ids = records
            .iter()
            .filter(|record| record.quality_metrics.total_runs == 0)
            .map(|record| record.id.clone())
            .collect::<Vec<_>>();

        SkillGovernanceReport {
            duplicate_pairs,
            low_quality_skill_ids,
            cold_skill_ids,
        }
    }

    pub async fn persist_if_dirty(&mut self) -> Result<()> {
        if !self.dirty {
            return Ok(());
        }
        let payload = SkillRegistryPersisted {
            skills: self.skills.values().cloned().collect(),
        };
        let bytes = serde_json::to_vec_pretty(&payload)
            .map_err(|err| miette!("serialize skill registry failed: {err}"))?;
        if let Some(parent) = self.storage_path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|err| {
                miette!(
                    "create skill registry parent directory {} failed: {err}",
                    parent.display()
                )
            })?;
        }
        tokio::fs::write(&self.storage_path, bytes)
            .await
            .map_err(|err| {
                miette!(
                    "write skill registry {} failed: {err}",
                    self.storage_path.display()
                )
            })?;
        self.dirty = false;
        Ok(())
    }

    pub async fn shutdown(mut self) {
        if let Err(err) = self.persist_if_dirty().await {
            tracing::warn!("persist skill registry failed during shutdown: {err:?}");
        }
    }

    async fn load_from_disk(&mut self) {
        let Ok(bytes) = tokio::fs::read(&self.storage_path).await else {
            return;
        };
        let Ok(persisted) = serde_json::from_slice::<SkillRegistryPersisted>(&bytes) else {
            tracing::warn!(
                "failed to parse skill registry {}; starting with empty registry",
                self.storage_path.display()
            );
            return;
        };

        for candidate in persisted.skills {
            match candidate.normalize() {
                Ok(record) => {
                    self.skills.insert(record.id.clone(), record);
                }
                Err(err) => {
                    tracing::warn!("dropping invalid skill while loading registry: {err:?}");
                }
            }
        }
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

fn slugify_skill_name(name: &str) -> String {
    normalize_identifier(name)
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

fn name_similarity(left: &str, right: &str) -> f64 {
    let left_tokens = tokenize(left);
    let right_tokens = tokenize(right);
    if left_tokens.is_empty() || right_tokens.is_empty() {
        return 0.0;
    }
    let left_set = left_tokens.into_iter().collect::<HashSet<_>>();
    let right_set = right_tokens.into_iter().collect::<HashSet<_>>();
    let intersection = left_set.intersection(&right_set).count() as f64;
    let union = left_set.union(&right_set).count() as f64;
    if union <= 0.0 {
        0.0
    } else {
        intersection / union
    }
}

fn tokenize(text: &str) -> Vec<String> {
    text.split(|ch: char| !ch.is_alphanumeric())
        .map(|token| token.trim().to_ascii_lowercase())
        .filter(|token| !token.is_empty())
        .collect()
}

fn score_skill(record: &SkillRecord, tokens: &[String]) -> f64 {
    let name = record.name.to_ascii_lowercase();
    let mut haystack = String::new();
    haystack.push_str(&name);
    haystack.push(' ');
    haystack.push_str(&record.id.to_ascii_lowercase());
    haystack.push(' ');
    haystack.push_str(&record.trigger_conditions.join(" ").to_ascii_lowercase());
    haystack.push(' ');
    haystack.push_str(&record.workflow_steps.join(" ").to_ascii_lowercase());
    haystack.push(' ');
    haystack.push_str(&record.done_criteria.join(" ").to_ascii_lowercase());

    let mut score = 0.0;
    for token in tokens {
        if name == *token {
            score += 3.0;
        } else if name.contains(token) {
            score += 2.0;
        } else if haystack.contains(token) {
            score += 1.0;
        }
    }
    score
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_skill() -> NewSkillRecord {
        NewSkillRecord {
            name: "Investigate runtime failures".to_string(),
            trigger_conditions: vec!["runtime error".to_string()],
            preconditions: vec!["logs available".to_string()],
            workflow_steps: vec![
                "collect evidence".to_string(),
                "locate root cause".to_string(),
            ],
            done_criteria: vec!["fix validated".to_string()],
            failure_recovery: vec!["rollback".to_string()],
        }
    }

    #[test]
    fn create_skill_assigns_slug_id() {
        let mut registry = SkillRegistry {
            storage_path: PathBuf::from("skills_registry.json"),
            skills: BTreeMap::new(),
            dirty: false,
        };

        let created = registry
            .create_skill(sample_skill())
            .expect("create skill should succeed");

        assert_eq!(created.id, "investigate-runtime-failures");
        assert_eq!(created.version, 1);
        assert!(registry.dirty);
    }

    #[test]
    fn query_returns_ranked_matches() {
        let mut registry = SkillRegistry {
            storage_path: PathBuf::from("skills_registry.json"),
            skills: BTreeMap::new(),
            dirty: false,
        };
        let first = registry
            .create_skill(sample_skill())
            .expect("create first skill");
        let second = registry
            .create_skill(NewSkillRecord {
                name: "Write release notes".to_string(),
                trigger_conditions: vec!["release".to_string()],
                preconditions: vec!["changeset ready".to_string()],
                workflow_steps: vec!["summarize changes".to_string()],
                done_criteria: vec!["notes published".to_string()],
                failure_recovery: vec![],
            })
            .expect("create second skill");

        let results = registry.query("runtime", 5);

        assert!(!results.is_empty());
        assert_eq!(results[0].summary.id, first.id);
        assert!(results.iter().all(|item| item.summary.id != second.id));
    }

    #[test]
    fn log_outcome_updates_metrics() {
        let mut registry = SkillRegistry {
            storage_path: PathBuf::from("skills_registry.json"),
            skills: BTreeMap::new(),
            dirty: false,
        };
        let created = registry
            .create_skill(sample_skill())
            .expect("create skill should succeed");

        let updated = registry
            .log_outcome(
                &created.id,
                SkillOutcomeLog {
                    success: true,
                    steps_executed: Some(4),
                    regression: false,
                    summary: Some("resolved incident".to_string()),
                    failure_type: None,
                },
            )
            .expect("log outcome should succeed");

        assert_eq!(updated.quality_metrics.total_runs, 1);
        assert_eq!(updated.quality_metrics.success_runs, 1);
        assert_eq!(updated.quality_metrics.failure_runs, 0);
        assert!(updated.quality_metrics.avg_steps >= 4.0);
        assert!(updated.version >= 2);
    }
}
