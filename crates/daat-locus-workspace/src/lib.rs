//! Shared workspace identities, immutable content and independently writable branches.
//!
//! `default` is the caller's ordinary directory. Named branches pin an immutable
//! base and share unchanged objects. Native projections are adapters over this
//! engine; the engine has no Actor, model, sandbox or administrator dependencies.
mod directory;
pub mod native;
mod projections;
mod store;
mod workspace;

pub use directory::{Access, Authorize, Directory};
pub use projections::{Location, Workspaces};
pub use store::{Collection, ObjectId, Statistics, Store};
pub use workspace::{Change, ChangeKind, Entry, EntryKind, MergeOutcome, Workspace};

use std::io;

pub const DEFAULT_BRANCH: &str = "default";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("filesystem authorization failed: {0}")]
    Authorization(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error(transparent)]
    Database(#[from] rusqlite::Error),
    #[error(transparent)]
    Encoding(#[from] serde_json::Error),
    #[error("invalid workspace input: {0}")]
    Invalid(String),
    #[error("workspace conflict: {0}")]
    Conflict(String),
    #[error("workspace object failed its content hash: {0}")]
    Corrupt(String),
    #[error("workspace backend is unavailable: {0}")]
    Unsupported(String),
}

pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn missing(message: impl Into<String>) -> Error {
    io::Error::new(io::ErrorKind::NotFound, message.into()).into()
}
