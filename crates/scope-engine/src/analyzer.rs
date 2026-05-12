use crate::api::PropagationResult;

/// Analyzer provides cross-file reference lookups.
/// Implemented by LSP-backed analyzers; tree-sitter handles only
/// line-number → symbol-name mapping (see `TreeSitterAnalyzer`).
pub trait Analyzer: Send + Sync {
    /// Find all references to the given symbol across the project.
    fn find_references(&self, symbol_name: &str) -> Vec<PropagationResult>;
}
