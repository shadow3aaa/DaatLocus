//! SCOPE engine in-process client.
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
use scope_engine::state::{PropagationState, ReadHandleRegistry};
use scope_engine::treesitter::TreeSitterAnalyzer;

/// In-process SCOPE engine client.
///
/// Wraps the scope-engine library to provide:
/// - Stable-handle code search and reading
/// - Hash-anchored code editing and deletion
/// - Propagation review events
/// - Tree-sitter symbol lookup
/// - Config hints for language servers
pub struct ScopeClient {
    project_root: Option<PathBuf>,
    propagation_state: Mutex<PropagationState>,
    read_handles: Mutex<ReadHandleRegistry>,
    lsp_analyzer: Mutex<Option<Box<dyn Analyzer + Send>>>,
    tree_sitter: TreeSitterAnalyzer,
}

impl ScopeClient {
    /// Create a new scope client (no project opened yet).
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self {
            project_root: None,
            propagation_state: Mutex::new(PropagationState::new()),
            read_handles: Mutex::new(ReadHandleRegistry::new()),
            lsp_analyzer: Mutex::new(None),
            tree_sitter: TreeSitterAnalyzer::new(),
        }
    }

    /// Open a project, setting the root directory for subsequent operations.
    #[allow(dead_code)]
    pub fn open_project(
        &mut self,
        project_root: impl Into<PathBuf>,
    ) -> Result<api::OpenProjectResponse> {
        let project_root = project_root.into();
        let previous_project_root = self.project_root.clone();
        let response = engine::open_project(
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
            self.read_handles
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clear();
        }
        self.project_root = Some(project_root);
        Ok(response)
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

    /// Read code using a stable search handle.
    #[allow(dead_code)]
    pub fn read_code(&self, request: api::ReadCodeRequest) -> Result<api::ReadCodeResponse> {
        let root = self.require_project_root()?;
        let handles = self
            .read_handles
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        engine::read_code(root, &request, &handles)
            .map_err(|err| miette!("scope-engine read_code failed: {err}"))
    }

    /// Search code and return stable read handles.
    #[allow(dead_code)]
    pub fn search_code(
        &self,
        query: &str,
        path: Option<&str>,
        include: Option<&str>,
        limit: Option<usize>,
    ) -> Result<api::SearchCodeResponse> {
        let root = self.require_project_root()?;
        let mut handles = self
            .read_handles
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        engine::search_code(
            root,
            &api::SearchCodeRequest {
                query: query.to_string(),
                path: path.map(str::to_string),
                include: include.map(str::to_string),
                limit,
            },
            &mut handles,
        )
        .map_err(|err| miette!("scope-engine search_code failed: {err}"))
    }

    /// Apply structured edits via scope-engine.
    #[allow(dead_code)]
    pub fn edit_code(&self, edits: &[api::StructuredEdit]) -> Result<Vec<api::PropagationResult>> {
        let root = self.require_project_root()?;
        engine::edit_code(
            root,
            &api::EditCodeRequest {
                edits: edits.to_vec(),
            },
            &self.propagation_state,
            &self.lsp_analyzer,
        )
        .map(|response| response.propagation_results)
        .map_err(|err| miette!("scope-engine edit_code failed: {err}"))
    }

    /// Return whether SCOPE owns semantic source operations for a path.
    #[allow(dead_code)]
    pub fn is_responsible_source(&self, path: &Path) -> Result<api::IsResponsibleSourceResponse> {
        let root = self.require_project_root()?;
        engine::is_responsible_source(
            root,
            &api::IsResponsibleSourceRequest {
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
    pub fn ack_next_events(&self, limit: Option<usize>) -> api::NextReviewResponse {
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

    /// Return SCOPE-owned usage documentation and model-facing protocol schema.
    #[allow(dead_code)]
    pub fn usage() -> api::ScopeUsageResponse {
        scope_engine::usage::usage_response()
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

impl Default for ScopeClient {
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

        let mut client = ScopeClient::new();
        assert!(
            client
                .next_review_event(vec![open_result("src/main.rs::fn main")])
                .is_some()
        );
        assert_eq!(client.pending_review_count(), 0);
        assert!(
            client
                .next_review_event(vec![open_result("src/main.rs::fn main")])
                .is_some()
        );
        assert_eq!(client.pending_review_count(), 0);

        client
            .propagation_state
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .accumulate(vec![open_result("src/main.rs::fn main")]);
        assert_eq!(client.pending_review_count(), 1);

        client.open_project(temp_dir.path()).expect("open project");
        assert_eq!(client.pending_review_count(), 0);

        client
            .propagation_state
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .accumulate(vec![open_result("src/lib.rs::fn lib")]);
        assert_eq!(client.pending_review_count(), 1);

        let response = client
            .open_project(temp_dir.path())
            .expect("reopen same project");
        assert_eq!(response.status, "already_open");
        assert_eq!(client.pending_review_count(), 1);

        client
            .open_project(other_temp_dir.path())
            .expect("open other project");
        assert_eq!(client.pending_review_count(), 0);
        assert!(
            client
                .next_review_event(vec![open_result("src/main.rs::fn main")])
                .is_some()
        );
    }

    #[test]
    fn ack_next_events_returns_batch_and_remaining_count() {
        let client = ScopeClient::new();
        client
            .propagation_state
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .accumulate(vec![
                open_result("src/a.rs::fn first"),
                open_result("src/b.rs::fn second"),
                open_result("src/c.rs::fn third"),
            ]);

        let response = client.ack_next_events(Some(2));

        assert_eq!(response.returned, 2);
        assert_eq!(response.reviews.len(), 2);
        assert_eq!(response.remaining, 1);
        match response.review.unwrap() {
            api::ReviewEvent::InvestigateImpact {
                modified_symbol, ..
            } => assert_eq!(modified_symbol, "src/c.rs::fn third"),
            _ => panic!("expected InvestigateImpact review"),
        }
    }
}
