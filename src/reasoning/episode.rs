use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EpisodeActionRecord {
    pub kind: String,
    pub summary: String,
}
