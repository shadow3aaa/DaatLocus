//! In-process SCOPE engine handle.
//!
//! Provides direct access to scope-engine functionality (tree-sitter parsing,
//! symbol lookup, code editing, propagation analysis) without spawning a
//! separate helper process. The scope-engine crate is linked directly as a
//! library dependency.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use miette::{Result, miette};
use scope_engine::analyzer::Analyzer;
use scope_engine::api;
use scope_engine::engine;
use scope_engine::language::LanguageRegistry;
use scope_engine::state::PropagationState;
use scope_engine::treesitter::TreeSitterAnalyzer;

pub struct ScopeEditCodeResult {
    pub propagation_results: Vec<api::PropagationResult>,
    pub applied_summary: api::AppliedStructuredEditSummary,
}

/// In-process SCOPE engine handle.
///
/// Wraps the scope-engine library to provide:
/// - Path plus line-hash code search and reading
/// - Hash-anchored code editing and deletion
/// - Propagation review events
/// - Tree-sitter symbol lookup
/// - Config hints for language servers
pub struct ScopeEngineHandle {
    project_root: Option<PathBuf>,
    propagation_state: Mutex<PropagationState>,
    lsp_analyzer: Mutex<Option<Box<dyn Analyzer + Send>>>,
    tree_sitter: TreeSitterAnalyzer,
}

impl ScopeEngineHandle {
    /// Create a new scope engine handle (no project opened yet).
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self {
            project_root: None,
            propagation_state: Mutex::new(PropagationState::new()),
            lsp_analyzer: Mutex::new(None),
            tree_sitter: TreeSitterAnalyzer::new(),
        }
    }

    /// Open a project, setting the root directory for subsequent operations.
    #[allow(dead_code)]
    pub fn open_project(
        &mut self,
        project_root: impl Into<PathBuf>,
    ) -> Result<api::OpenProjectOutput> {
        let project_root = project_root.into();
        let previous_project_root = self.project_root.clone();
        let output = engine::open_project(
            &project_root,
            previous_project_root.as_deref(),
            &self.lsp_analyzer,
        )
        .map_err(|err| miette!("{err}"))?;
        if previous_project_root.as_deref() != Some(project_root.as_path()) {
            let mut state = self
                .propagation_state
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            *state = PropagationState::new();
        }
        self.project_root = Some(project_root);
        Ok(output)
    }

    /// The project root path, if a project has been opened.
    #[allow(dead_code)]
    pub fn project_root(&self) -> Option<&Path> {
        self.project_root.as_deref()
    }

    fn require_project_root(&self) -> Result<&Path> {
        self.project_root
            .as_deref()
            .ok_or_else(|| miette!("no project opened"))
    }

    /// Accumulate propagation results and get the next review event, if any.
    #[allow(dead_code)]
    pub fn next_review_event(
        &self,
        results: Vec<api::PropagationResult>,
    ) -> Option<api::ReviewEvent> {
        let mut state = self
            .propagation_state
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        state.accumulate(results);
        state.next_review()
    }

    /// Find the containing symbol for a given file and line number using tree-sitter.
    #[allow(dead_code)]
    pub fn find_containing_symbol(&self, file_path: &Path, line_number: usize) -> Option<String> {
        let root = self.project_root.as_deref()?;
        self.tree_sitter
            .find_containing_symbol(file_path, line_number, root)
    }

    /// Read code using a path plus line-hash anchor.
    #[allow(dead_code)]
    pub fn read_code(&self, input: api::ReadCodeInput) -> Result<api::ReadCodeOutput> {
        let root = self.require_project_root()?;
        engine::read_code(root, &input)
            .map_err(|err| miette!("scope-engine read_code failed: {err}"))
    }

    /// Search code and return matched line-hash hits.
    #[allow(dead_code)]
    pub fn search_code(&self, input: api::SearchCodeInput) -> Result<api::SearchCodeOutput> {
        let root = self.require_project_root()?;
        engine::search_code(root, &input)
            .map_err(|err| miette!("scope-engine search_code failed: {err}"))
    }

    /// Apply structured edits via scope-engine.
    #[allow(dead_code)]
    pub fn edit_code(&self, edits: &[api::StructuredEdit]) -> Result<ScopeEditCodeResult> {
        let root = self.require_project_root()?;
        let output = engine::edit_code(
            root,
            &api::EditCodeInput {
                edits: edits.to_vec(),
            },
            &self.propagation_state,
            &self.lsp_analyzer,
        )
        .map_err(|err| miette!("scope-engine edit_code failed: {err}"))?;
        Ok(ScopeEditCodeResult {
            propagation_results: output.propagation_results,
            applied_summary: output.applied_summary,
        })
    }

    /// Return whether SCOPE owns semantic source operations for a path.
    #[allow(dead_code)]
    pub fn is_responsible_source(&self, path: &Path) -> Result<api::SourceResponsibility> {
        let root = self.require_project_root()?;
        engine::is_responsible_source(
            root,
            &api::SourceResponsibilityInput {
                path: path.to_string_lossy().into_owned(),
            },
        )
        .map_err(|err| miette!("scope-engine is_responsible_source failed: {err}"))
    }

    /// Count accumulated propagation review events.
    #[allow(dead_code)]
    pub fn pending_review_count(&self) -> usize {
        let state = self
            .propagation_state
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        state.pending_count()
    }

    /// Acknowledge and return accumulated propagation review events.
    #[allow(dead_code)]
    pub fn ack_next_events(&self, limit: Option<usize>) -> api::ReviewBatch {
        engine::ack_next_events(&self.propagation_state, limit)
            .unwrap_or_else(|err| panic!("scope-engine ack_next_events failed: {err}"))
    }

    /// Get the next accumulated propagation review event, if any.
    #[allow(dead_code)]
    pub fn ack_next_event(&self) -> Option<api::ReviewEvent> {
        self.ack_next_events(None).review
    }

    /// Get config hints for language servers and tree-sitter languages.
    #[allow(dead_code)]
    pub fn get_config_hints() -> serde_json::Value {
        engine::config_hints()
    }

    /// Get the list of supported tree-sitter languages.
    #[allow(dead_code)]
    pub fn supported_languages() -> Vec<(String, Vec<String>)> {
        let registry = LanguageRegistry::new();
        registry
            .list_languages()
            .into_iter()
            .map(|(name, exts)| {
                (
                    name.to_string(),
                    exts.iter().map(|e| e.to_string()).collect(),
                )
            })
            .collect()
    }
}

impl Default for ScopeEngineHandle {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_result(selector: &str) -> api::PropagationResult {
        api::PropagationResult {
            selector: selector.to_string(),
            reason: "changed".to_string(),
            source: api::PropagationSource::OpenEnded,
            lsp_references: None,
            diff_summary: Some("diff".to_string()),
            file_snippet: Some("fn main() {}".to_string()),
            project_files: Some(vec!["src/main.rs".to_string()]),
        }
    }

    #[test]
    fn open_project_preserves_pending_review_for_same_project_and_resets_on_project_change() {
        let temp_dir = tempfile::tempdir().unwrap();
        let other_temp_dir = tempfile::tempdir().unwrap();
        std::fs::write(
            temp_dir.path().join("Cargo.toml"),
            "[package]\nname = \"tmp\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        std::fs::write(
            other_temp_dir.path().join("Cargo.toml"),
            "[package]\nname = \"other\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();

        let mut handle = ScopeEngineHandle::new();
        assert!(
            handle
                .next_review_event(vec![open_result("src/main.rs::fn main")])
                .is_some()
        );
        assert_eq!(handle.pending_review_count(), 0);
        assert!(
            handle
                .next_review_event(vec![open_result("src/main.rs::fn main")])
                .is_some()
        );
        assert_eq!(handle.pending_review_count(), 0);

        handle
            .propagation_state
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .accumulate(vec![open_result("src/main.rs::fn main")]);
        assert_eq!(handle.pending_review_count(), 1);

        handle.open_project(temp_dir.path()).expect("open project");
        assert_eq!(handle.pending_review_count(), 0);

        handle
            .propagation_state
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .accumulate(vec![open_result("src/lib.rs::fn lib")]);
        assert_eq!(handle.pending_review_count(), 1);

        let output = handle
            .open_project(temp_dir.path())
            .expect("reopen same project");
        assert_eq!(output.status, "already_open");
        assert_eq!(handle.pending_review_count(), 1);

        handle
            .open_project(other_temp_dir.path())
            .expect("open other project");
        assert_eq!(handle.pending_review_count(), 0);
        assert!(
            handle
                .next_review_event(vec![open_result("src/main.rs::fn main")])
                .is_some()
        );
    }

    #[test]
    fn ack_next_events_returns_batch_and_remaining_count() {
        let handle = ScopeEngineHandle::new();
        handle
            .propagation_state
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .accumulate(vec![
                open_result("src/a.rs::fn first"),
                open_result("src/b.rs::fn second"),
                open_result("src/c.rs::fn third"),
            ]);

        let output = handle.ack_next_events(Some(2));

        assert_eq!(output.returned, 2);
        assert_eq!(output.reviews.len(), 2);
        assert_eq!(output.remaining, 1);
        match output.review.unwrap() {
            api::ReviewEvent::InvestigateImpact {
                modified_symbol, ..
            } => assert_eq!(modified_symbol, "src/c.rs::fn third"),
            _ => panic!("expected InvestigateImpact review"),
        }
    }
}
