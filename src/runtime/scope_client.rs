//! SCOPE engine in-process client.
//!
//! Provides direct access to scope-engine functionality (tree-sitter parsing,
//! symbol lookup, code editing, propagation analysis) without spawning a
//! separate JSON-RPC process. The scope-engine crate is linked directly as
//! a library dependency.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use miette::{Result, miette};
use scope_engine::analyzer::Analyzer;
use scope_engine::api;
use scope_engine::language::LanguageRegistry;
use scope_engine::patch;
use scope_engine::selector;
use scope_engine::state::PropagationState;
use scope_engine::treesitter::TreeSitterAnalyzer;

/// In-process SCOPE engine client.
///
/// Wraps the scope-engine library to provide:
/// - Symbol-based code reading via selector
/// - Text-based code search
/// - Selector-based code editing and deletion
/// - Propagation review events
/// - Tree-sitter symbol lookup
/// - Config hints for language servers
pub struct ScopeClient {
    project_root: Option<PathBuf>,
    propagation_state: Mutex<PropagationState>,
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
            lsp_analyzer: Mutex::new(None),
            tree_sitter: TreeSitterAnalyzer::new(),
        }
    }

    /// Open a project, setting the root directory for subsequent operations.
    #[allow(dead_code)]
    pub fn open_project(
        &mut self,
        project_root: impl Into<PathBuf>,
        language: Option<&str>,
    ) -> api::JsonRpcResponse {
        let project_root = project_root.into();
        let fake_req = api::JsonRpcRequest {
            _jsonrpc: "2.0".to_string(),
            id: serde_json::json!(1),
            method: "open_project".to_string(),
            params: serde_json::json!({
                "project_root": project_root.to_string_lossy(),
                "language": language,
            }),
        };
        let response = scope_engine::server::dispatch(
            &fake_req,
            Some(&project_root),
            &self.propagation_state,
            &self.lsp_analyzer,
        );
        if response.error.is_none() {
            let mut state = self
                .propagation_state
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            *state = PropagationState::new();
            self.project_root = Some(project_root);
        }
        response
    }

    /// The project root path, if a project has been opened.
    #[allow(dead_code)]
    pub fn project_root(&self) -> Option<&Path> {
        self.project_root.as_deref()
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

    /// Parse a selector string into a structured `ParsedSelector`.
    #[allow(dead_code)]
    pub fn parse_selector(selector_str: &str) -> Result<selector::ParsedSelector, String> {
        selector::parse_selector(selector_str)
    }

    /// Resolve a selector's file path against the project root.
    #[allow(dead_code)]
    pub fn resolve_file(
        &self,
        parsed: &selector::ParsedSelector,
    ) -> Result<(PathBuf, String), String> {
        let root = self.project_root.as_deref().ok_or("no project opened")?;
        selector::resolve_file(parsed, root)
    }

    /// Read a selector's file content using scope-engine selector resolution.
    #[allow(dead_code)]
    pub fn read_code(&self, selector_str: &str) -> Result<api::ReadCodeResponse> {
        let result = self.dispatch("read_code", serde_json::json!({ "selector": selector_str }))?;
        serde_json::from_value(result).map_err(|err| miette!("invalid read_code response: {err}"))
    }

    /// Search code using scope-engine's JSON-RPC dispatch path.
    #[allow(dead_code)]
    pub fn search_code(
        &self,
        query: &str,
        limit: Option<usize>,
    ) -> Result<api::SearchCodeResponse> {
        let result = self.dispatch(
            "search_code",
            serde_json::json!({
                "query": query,
                "limit": limit,
            }),
        )?;
        serde_json::from_value(result).map_err(|err| miette!("invalid search_code response: {err}"))
    }

    /// Search code with opencode-style grep parameters.
    #[allow(dead_code)]
    pub fn grep_code(
        &self,
        pattern: &str,
        path: Option<&str>,
        include: Option<&str>,
    ) -> Result<api::GrepCodeResponse> {
        let result = self.dispatch(
            "grep_code",
            serde_json::json!({
                "pattern": pattern,
                "path": path,
                "include": include,
            }),
        )?;
        serde_json::from_value(result).map_err(|err| miette!("invalid grep_code response: {err}"))
    }

    /// Find files with opencode-style glob parameters.
    #[allow(dead_code)]
    pub fn glob_files(&self, pattern: &str, path: Option<&str>) -> Result<api::GlobFilesResponse> {
        let result = self.dispatch(
            "glob_files",
            serde_json::json!({
                "pattern": pattern,
                "path": path,
            }),
        )?;
        serde_json::from_value(result).map_err(|err| miette!("invalid glob_files response: {err}"))
    }

    /// Apply a complete SCOPE Diff patch via scope-engine.
    #[allow(dead_code)]
    pub fn edit_code(&self, diff: &str) -> Result<Vec<api::PropagationResult>> {
        let root = self
            .project_root
            .as_deref()
            .ok_or_else(|| miette!("no project opened"))?;
        let results = patch::edit_code_apply(diff, root, &self.lsp_analyzer)
            .map_err(|err| miette!("{err}"))?;
        if !results.is_empty() {
            let mut state = self
                .propagation_state
                .lock()
                .unwrap_or_else(|err| err.into_inner());
            state.accumulate(results.clone());
        }
        Ok(results)
    }

    /// Return whether SCOPE owns semantic source operations for a path.
    #[allow(dead_code)]
    pub fn is_responsible_source(&self, path: &Path) -> Result<api::IsResponsibleSourceResponse> {
        let result = self.dispatch(
            "is_responsible_source",
            serde_json::json!({ "path": path.to_string_lossy() }),
        )?;
        serde_json::from_value(result)
            .map_err(|err| miette!("invalid is_responsible_source response: {err}"))
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
        const DEFAULT_LIMIT: usize = 1;
        const MAX_LIMIT: usize = 100;

        let limit = limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
        let mut state = self
            .propagation_state
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let reviews = state.next_reviews(limit);
        let review = reviews.first().cloned();
        let returned = reviews.len();
        let remaining = state.pending_count();

        api::NextReviewResponse {
            review,
            reviews,
            returned,
            remaining,
        }
    }

    /// Get the next accumulated propagation review event, if any.
    #[allow(dead_code)]
    pub fn ack_next_event(&self) -> Option<api::ReviewEvent> {
        self.ack_next_events(None).review
    }

    fn dispatch(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
        let root = self
            .project_root
            .as_deref()
            .ok_or_else(|| miette!("no project opened"))?;
        let fake_req = api::JsonRpcRequest {
            _jsonrpc: "2.0".to_string(),
            id: serde_json::json!(1),
            method: method.to_string(),
            params,
        };
        let response = scope_engine::server::dispatch(
            &fake_req,
            Some(root),
            &self.propagation_state,
            &self.lsp_analyzer,
        );
        if let Some(error) = response.error {
            return Err(miette!("scope-engine {method} failed: {}", error.message));
        }
        Ok(response.result.unwrap_or(serde_json::Value::Null))
    }

    /// Get config hints for language servers and tree-sitter languages.
    ///
    /// Returns a JSON-RPC response containing `tree_sitter_languages` and `lsp_languages`.
    #[allow(dead_code)]
    pub fn get_config_hints() -> api::JsonRpcResponse {
        let fake_id = serde_json::json!(1);
        let fake_req = api::JsonRpcRequest {
            _jsonrpc: "2.0".to_string(),
            id: fake_id,
            method: "get_config_hints".to_string(),
            params: serde_json::Value::Null,
        };
        scope_engine::server::dispatch_get_config_hints(&fake_req)
    }

    /// Return SCOPE-owned usage documentation and selector schema.
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
    fn open_project_resets_pending_review_state() {
        let temp_dir = tempfile::tempdir().unwrap();
        std::fs::write(
            temp_dir.path().join("Cargo.toml"),
            "[package]\nname = \"tmp\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
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

        let response = client.open_project(temp_dir.path(), Some("rust"));
        assert!(response.error.is_none());
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
