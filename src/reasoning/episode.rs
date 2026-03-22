use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::environment::EpisodeObservation;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpisodeTask {
    pub id: String,
    pub source: String,
    pub title: String,
    pub instruction: String,
    pub workspace_hint: Option<String>,
    pub setup_commands: Vec<String>,
    #[serde(default)]
    pub validation_commands: Vec<String>,
    pub success_criteria: Vec<String>,
    pub max_steps: usize,
    pub tags: Vec<String>,
    pub metadata: BTreeMap<String, String>,
    #[serde(default)]
    pub task_goal: Option<String>,
    #[serde(default)]
    pub investigation_plan: Vec<String>,
    #[serde(default)]
    pub done_criteria: Vec<String>,
    #[serde(default)]
    pub key_anchors: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpisodeActionRecord {
    pub kind: String,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpisodeStep {
    pub index: usize,
    pub module: String,
    pub action: EpisodeActionRecord,
    pub observation_summary: String,
    pub snapshot_text: String,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EpisodeStatus {
    InProgress,
    Succeeded,
    Failed,
    Aborted,
    MaxStepsExceeded,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EpisodeMetric {
    pub success: bool,
    pub score: f32,
    pub steps_used: usize,
    #[serde(default)]
    pub repeated_actions: usize,
    pub stagnation_events: usize,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpisodeOutcome {
    pub task: EpisodeTask,
    pub environment_name: String,
    pub initial_observation: EpisodeObservation,
    pub final_observation: EpisodeObservation,
    pub status: EpisodeStatus,
    pub steps: Vec<EpisodeStep>,
    pub metric: EpisodeMetric,
}
