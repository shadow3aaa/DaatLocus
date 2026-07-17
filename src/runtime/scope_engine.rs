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
use scope_engine::state::PropagationState;

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
/// - Config hints for language servers
pub struct ScopeEngineHandle {
    project_root: Option<PathBuf>,
    propagation_state: Mutex<PropagationState>,
    lsp_analyzer: Mutex<Option<Box<dyn Analyzer + Send>>>,
}

impl ScopeEngineHandle {
    pub fn new() -> Self {
        Self {
            project_root: None,
            propagation_state: Mutex::new(PropagationState::new()),
            lsp_analyzer: Mutex::new(None),
        }
    }

    /// Open a project, setting the root directory for subsequent operations.
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
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *state = PropagationState::new();
        }
        self.project_root = Some(project_root);
        Ok(output)
    }

    fn require_project_root(&self) -> Result<&Path> {
        self.project_root
            .as_deref()
            .ok_or_else(|| miette!("no project opened"))
    }

    /// Accumulate propagation results and get the next review event, if any.
    #[cfg(test)]
    pub fn next_review_event(
        &self,
        results: Vec<api::PropagationResult>,
    ) -> Option<api::ReviewEvent> {
        let mut state = self
            .propagation_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.accumulate(results);
        state.next_review()
    }

    /// Read code using a path plus line-hash anchor.
    pub fn read_code(&self, input: &api::ReadCodeInput) -> Result<api::ReadCodeOutput> {
        let root = self.require_project_root()?;
        engine::read_code(root, input)
            .map_err(|err| miette!("scope-engine read_code failed: {err}"))
    }

    /// Search code and return matched line-hash hits.
    pub fn search_code(&self, input: &api::SearchCodeInput) -> Result<api::SearchCodeOutput> {
        let root = self.require_project_root()?;
        engine::search_code(root, input)
            .map_err(|err| miette!("scope-engine search_code failed: {err}"))
    }

    /// Apply structured edits via scope-engine.
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
    pub fn pending_review_count(&self) -> usize {
        let state = self
            .propagation_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.pending_count()
    }

    /// Acknowledge and return accumulated propagation review events.
    pub fn ack_next_events(&self, limit: Option<usize>) -> api::ReviewBatch {
        engine::ack_next_events(&self.propagation_state, limit)
            .unwrap_or_else(|err| panic!("scope-engine ack_next_events failed: {err}"))
    }

    /// Get config hints for language servers and tree-sitter languages.
    pub fn get_config_hints() -> serde_json::Value {
        engine::config_hints()
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
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .accumulate(vec![open_result("src/main.rs::fn main")]);
        assert_eq!(handle.pending_review_count(), 1);

        handle.open_project(temp_dir.path()).expect("open project");
        assert_eq!(handle.pending_review_count(), 0);

        handle
            .propagation_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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
            api::ReviewEvent::KnownReferences { .. } => panic!("expected InvestigateImpact review"),
        }
    }
}
