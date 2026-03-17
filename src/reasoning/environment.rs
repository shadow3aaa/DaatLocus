use std::collections::BTreeMap;

use async_trait::async_trait;
use miette::Result;
use serde::{Deserialize, Serialize};

use crate::core::Effect;

use super::episode::{EpisodeMetric, EpisodeStatus, EpisodeStep, EpisodeTask};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EpisodeObservation {
    pub summary: String,
    pub snapshot_text: String,
    pub metadata: BTreeMap<String, String>,
}

#[async_trait]
pub trait EpisodeEnvironment {
    fn name(&self) -> &'static str;

    async fn reset(&mut self, task: &EpisodeTask) -> Result<EpisodeObservation>;

    async fn apply_effect(&mut self, effect: &Effect) -> Result<EpisodeObservation>;

    fn status(
        &self,
        task: &EpisodeTask,
        latest_observation: &EpisodeObservation,
        steps: &[EpisodeStep],
    ) -> EpisodeStatus;

    fn metric(
        &self,
        task: &EpisodeTask,
        latest_observation: &EpisodeObservation,
        steps: &[EpisodeStep],
        status: EpisodeStatus,
    ) -> EpisodeMetric;
}
