use crate::api::PropagationResult;
use std::path::Path;

/// Analyzer provides cross-file reference lookups via LSP or other language servers.
///
/// Each language server implementation (rust-analyzer, gopls, etc.) implements this trait,
/// allowing scope-engine to query references without knowing the specific LSP server details.
///
/// All methods use `&self` to support interior mutability patterns (e.g. RefCell),
/// so implementations can be shared behind `Mutex<dyn Analyzer + Send>`.
pub trait Analyzer: Send {
    /// Find all references to the symbol at the given position in the file.
    ///
    /// `file_path` is the absolute path to the file.
    /// `line` is 1-based line number.
    /// `character` is 0-based column offset.
    /// `project_root` is the absolute path to the project root.
    fn find_references_for_symbol(
        &self,
        file_path: &Path,
        line: usize,
        character: usize,
        project_root: &Path,
    ) -> Vec<PropagationResult>;

    /// Notify the language server that a file was opened.
    fn notify_did_open(&self, file_path: &Path, text: &str);

    /// Notify the language server that a file was modified (full sync).
    fn notify_did_change(&self, file_path: &Path, version: i32, text: &str);

    /// Notify the language server that a file was closed.
    fn notify_did_close(&self, file_path: &Path);

    /// Whether the analyzer is available and initialized.
    fn is_initialized(&self) -> bool;
}
