//! Decentralized account credentials. No bearer credentials or password plaintext.
mod bootstrap;
pub mod configuration;
pub mod login_error;
pub mod network;
pub mod public_connection;
mod relay;
mod store;
pub(crate) use bootstrap::local_owner_connection;
pub use store::{AccountStore, CredentialBundle, Metadata, normalize_name};
