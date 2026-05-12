use crate::analyzer::Analyzer;
use crate::api::{PropagationResult, PropagationSource};

pub struct LspAnalyzer;

impl LspAnalyzer {
    /// Create a new LspAnalyzer. Currently a placeholder.
    /// TODO: spawn and communicate with an actual LSP server (e.g. rust-analyzer).
    pub fn new(_project_root: &str, _language: &str) -> Self {
        Self
    }
}

impl Analyzer for LspAnalyzer {
    /// Placeholder: returns empty, meaning "no LSP available".
    /// When LSP integration is implemented, this will send
    /// textDocument/references requests to the LSP server and
    /// map results back to PropagationResult with source: Lsp
    /// and lsp_references populated with precise locations.
    fn find_references(&self, _symbol_name: &str) -> Vec<PropagationResult> {
        vec![]
    }
}
