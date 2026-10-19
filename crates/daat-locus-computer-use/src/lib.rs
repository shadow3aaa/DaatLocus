//! Native background GUI host. Independent of Actors, account storage, WebUI and Tauri.
pub use daat_locus_client_protocol::computer_use::{Error, Permissions, Reply, Request, Result};
#[cfg(target_os = "macos")]
use serde_json::Value;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(all(target_os = "macos", feature = "native-contract"))]
pub use macos::run_native_contract;
#[cfg(target_os = "macos")]
pub use macos::{Server, configure, permissions, request_permission};
