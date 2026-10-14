//! Actual loopback HTTP integration tests, organized by contract.
#[path = "http/account_api.rs"]
mod account;
#[path = "http/approvals_api.rs"]
mod approvals;
#[path = "http/configuration_api.rs"]
mod configuration;
#[path = "http/conversation_api.rs"]
mod conversation;
#[path = "http/devices_api.rs"]
mod devices;
#[path = "http/resources_api.rs"]
mod resources;
#[path = "http/stream_api.rs"]
mod stream;
mod support;
