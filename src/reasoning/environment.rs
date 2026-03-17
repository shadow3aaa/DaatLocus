use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EpisodeObservation {
    pub summary: String,
    pub snapshot_text: String,
    pub metadata: BTreeMap<String, String>,
}
