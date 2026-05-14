//! SCOPE — Semantic Code Operation & Propagation Engine
//!
//! This crate provides code-structure parsing, symbol lookup, LSP integration,
//! and propagation analysis. It can be used as a library (in-process) or as
//! a JSON-RPC stdio server (via the `scope-engine` binary).

pub mod analyzer;
pub mod api;
pub mod language;
pub mod lsp;
pub mod patch;
pub mod selector;
pub mod server;
pub mod state;
pub mod treesitter;
pub mod usage;
