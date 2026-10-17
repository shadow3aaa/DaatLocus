//! Authenticated host infrastructure. Actors and Apps never select a transport.
pub mod approval_index;
pub mod blob;
pub mod channel;
pub mod connection;
pub mod coordination;
pub mod core_wire;
pub mod delivery;
mod device_name;
pub mod host;
pub mod identity;
pub mod journal;
pub mod ledger;
pub mod observation;
pub mod peer;
pub mod protocol;
pub mod receiver;
pub mod recovery;
pub mod reset;
pub mod revocation;
pub mod runtime;
pub mod scheduler;
pub mod shared;
pub mod shared_apps;
pub mod shared_runtime;
pub mod shared_snapshot;
pub mod shared_store;
pub mod signaling;
pub mod sync;
pub mod transport;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StageId(pub Uuid);
impl StageId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}
impl Default for StageId {
    fn default() -> Self {
        Self::new()
    }
}
impl std::fmt::Display for StageId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActorAddress {
    pub stage_id: StageId,
    pub actor_id: String,
}
impl ActorAddress {
    pub fn validate(&self) -> Result<()> {
        if self.stage_id.0.is_nil()
            || self.actor_id.is_empty()
            || self.actor_id.len() > 128
            || !self
                .actor_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        {
            return Err(StageError::Invalid("Invalid Actor address"));
        }
        Ok(())
    }
    pub fn conversation_url(&self) -> String {
        format!("/stages/{}/actors/{}", self.stage_id, self.actor_id)
    }
}

#[derive(Debug, thiserror::Error, miette::Diagnostic)]
pub enum StageError {
    #[error("{0}")]
    Invalid(&'static str),
    #[error("{0}")]
    Forbidden(&'static str),
    #[error("{0}")]
    ApprovalDenied(String),
    #[error("{0}")]
    Conflict(&'static str),
    #[error("{0}")]
    Unavailable(&'static str),
    #[error("{0}")]
    Capacity(&'static str),
    #[error("Stage storage failed")]
    Storage(#[source] std::io::Error),
    #[error("Stage database failed")]
    Database(#[source] rusqlite::Error),
}
pub type Result<T> = std::result::Result<T, StageError>;
impl From<std::io::Error> for StageError {
    fn from(error: std::io::Error) -> Self {
        Self::Storage(error)
    }
}
impl From<rusqlite::Error> for StageError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Database(error)
    }
}

pub fn digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}
