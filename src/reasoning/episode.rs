use std::collections::BTreeMap;

use miette::Result;
use serde::{Deserialize, Serialize};

use crate::core::Effect;

use super::environment::{EpisodeEnvironment, EpisodeObservation};

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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpisodeStep {
    pub index: usize,
    pub module: String,
    pub effect: Effect,
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
    pub repeated_effects: usize,
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

pub async fn run_scripted_episode<E, I>(
    environment: &mut E,
    task: EpisodeTask,
    scripted_steps: I,
) -> Result<EpisodeOutcome>
where
    E: EpisodeEnvironment + Send,
    I: IntoIterator<Item = (String, Effect)>,
{
    let initial_observation = environment.reset(&task).await?;
    let mut latest_observation = initial_observation.clone();
    let mut steps = Vec::new();
    let mut status = environment.status(&task, &latest_observation, &steps);

    for (index, (module, effect)) in scripted_steps.into_iter().enumerate() {
        if index >= task.max_steps {
            status = EpisodeStatus::MaxStepsExceeded;
            break;
        }
        if status != EpisodeStatus::InProgress {
            break;
        }

        latest_observation = environment.apply_effect(&effect).await?;
        steps.push(EpisodeStep {
            index,
            module,
            effect,
            observation_summary: latest_observation.summary.clone(),
            snapshot_text: latest_observation.snapshot_text.clone(),
            metadata: latest_observation.metadata.clone(),
        });
        status = environment.status(&task, &latest_observation, &steps);
    }

    if status == EpisodeStatus::InProgress && steps.len() >= task.max_steps {
        status = EpisodeStatus::MaxStepsExceeded;
    }

    let metric = environment.metric(&task, &latest_observation, &steps, status);

    Ok(EpisodeOutcome {
        task,
        environment_name: environment.name().to_string(),
        initial_observation,
        final_observation: latest_observation,
        status,
        steps,
        metric,
    })
}
