use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::episode::EpisodeTask;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpisodeTaskPreview {
    pub id: String,
    pub title: String,
    pub workspace_hint: Option<String>,
    pub success_criteria_count: usize,
    pub validation_command_count: usize,
    pub max_steps: usize,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpisodeBatchSummary {
    pub total_tasks: usize,
    pub avg_max_steps: f32,
    pub source_counts: BTreeMap<String, usize>,
    pub tag_counts: BTreeMap<String, usize>,
    pub preview: Vec<EpisodeTaskPreview>,
}

pub struct EpisodeHarness;

impl EpisodeHarness {
    pub fn summarize_tasks(tasks: &[EpisodeTask], preview_limit: usize) -> EpisodeBatchSummary {
        let total_tasks = tasks.len();
        let avg_max_steps = if total_tasks == 0 {
            0.0
        } else {
            tasks.iter().map(|task| task.max_steps as f32).sum::<f32>() / total_tasks as f32
        };

        let mut source_counts = BTreeMap::new();
        let mut tag_counts = BTreeMap::new();
        for task in tasks {
            *source_counts.entry(task.source.clone()).or_insert(0) += 1;
            for tag in &task.tags {
                *tag_counts.entry(tag.clone()).or_insert(0) += 1;
            }
        }

        let preview = tasks
            .iter()
            .take(preview_limit)
            .map(|task| EpisodeTaskPreview {
                id: task.id.clone(),
                title: task.title.clone(),
                workspace_hint: task.workspace_hint.clone(),
                success_criteria_count: task.success_criteria.len(),
                validation_command_count: task.validation_commands.len(),
                max_steps: task.max_steps,
                tags: task.tags.clone(),
            })
            .collect();

        EpisodeBatchSummary {
            total_tasks,
            avg_max_steps,
            source_counts,
            tag_counts,
            preview,
        }
    }
}
