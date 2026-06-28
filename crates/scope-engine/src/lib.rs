//! SCOPE — Semantic Code Operation & Propagation Engine
//!
//! This crate provides code-structure parsing, symbol lookup, LSP integration,
//! and propagation analysis. It is an in-process library crate.

pub mod analyzer;
pub mod api;
pub mod engine;
pub mod language;
pub mod lsp;
pub mod patch;
pub mod selector;
pub mod state;
pub mod treesitter;
