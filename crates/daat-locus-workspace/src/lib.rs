//! Shared workspace branches, saved versions and native filesystem projections.
mod directory;
pub mod native;
mod projections;
#[cfg(windows)]
mod reparse;
mod store;
mod workspace;

pub use directory::{Access, Authorize, Directory};
pub use projections::{Location, Workspaces};
pub use store::{Collection, ObjectId, Statistics, Store};
pub use workspace::{
    CaptureProgress, Change, ChangeKind, Entry, EntryKind, FileVersion, MergeOutcome,
    PublishOutcome, Resolution, Storage, Version, Workspace,
};

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
