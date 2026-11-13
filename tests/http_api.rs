//! Actual loopback HTTP integration tests, organized by contract.
#[path = "http/account_api.rs"]
mod account;
#[path = "http/approvals_api.rs"]
mod approvals;
#[path = "http/apps_api.rs"]
mod apps;
#[path = "http/codex_api.rs"]
mod codex;
#[path = "http/configuration_api.rs"]
mod configuration;
#[path = "http/conversation_api.rs"]
mod conversation;
#[path = "http/devices_api.rs"]
mod devices;
#[path = "http/imports_api.rs"]
mod imports;
#[path = "http/prompt_api.rs"]
mod prompt;
#[path = "http/reset_api.rs"]
mod reset;
#[path = "http/resources_api.rs"]
mod resources;
#[path = "http/session_workers_api.rs"]
mod session_workers;
#[path = "http/sessions_api.rs"]
mod sessions;
#[path = "http/stream_api.rs"]
mod stream;
mod support;
#[path = "http/virtual_displays_api.rs"]
mod virtual_displays;
#[path = "http/websearch_api.rs"]
mod websearch;
