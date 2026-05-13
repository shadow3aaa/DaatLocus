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
        let parsed =
            selector::parse_selector(selector_str).map_err(|err| miette!("Bad selector: {err}"))?;
        let (full_path, _ext) = self.resolve_file(&parsed).map_err(|err| miette!("{err}"))?;
        let content = std::fs::read_to_string(&full_path)
            .map_err(|err| miette!("Failed to read {}: {err}", full_path.display()))?;

        Ok(api::ReadCodeResponse {
            selector: selector_str.to_string(),
            content,
            language: guess_language(&full_path).to_string(),
        })
    }

    /// Search code using scope-engine's JSON-RPC dispatch path.
    #[allow(dead_code)]
    pub fn search_code(&self, query: &str) -> Result<api::SearchCodeResponse> {
        let result = self.dispatch("search_code", serde_json::json!({ "query": query }))?;
        serde_json::from_value(result).map_err(|err| miette!("invalid search_code response: {err}"))
    }

    /// Apply a stripped v4a hunk-only patch via scope-engine.
    #[allow(dead_code)]
    pub fn edit_code(
        &self,
        selector_str: &str,
        patch_text: &str,
    ) -> Result<Vec<api::PropagationResult>> {
        let root = self
            .project_root
            .as_deref()
            .ok_or_else(|| miette!("no project opened"))?;
        let results = patch::edit_code_apply(selector_str, patch_text, root, &self.lsp_analyzer)
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

    /// Delete a selector via scope-engine.
    #[allow(dead_code)]
    pub fn delete_code(&self, selector_str: &str) -> Result<Vec<api::PropagationResult>> {
        let root = self
            .project_root
            .as_deref()
            .ok_or_else(|| miette!("no project opened"))?;
        let results = patch::delete_code_apply(selector_str, root, &self.lsp_analyzer)
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

    /// Count accumulated propagation review events.
    #[allow(dead_code)]
    pub fn pending_review_count(&self) -> usize {
        let state = self
            .propagation_state
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        state.pending_count()
    }

    /// Get the next accumulated propagation review event, if any.
    #[allow(dead_code)]
    pub fn ack_next_event(&self) -> Option<api::ReviewEvent> {
        let mut state = self
            .propagation_state
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        state.next_review()
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

fn guess_language(path: &Path) -> &'static str {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("rs") => "rust",
        Some("py") => "python",
        Some("js") | Some("mjs") | Some("cjs") => "javascript",
        Some("ts") | Some("mts") | Some("cts") => "typescript",
        Some("go") => "go",
        Some("java") => "java",
        Some("c") | Some("h") => "c",
        Some("cpp") | Some("cc") | Some("cxx") | Some("hpp") => "cpp",
        Some("toml") => "toml",
        Some("json") => "json",
        Some("yaml") | Some("yml") => "yaml",
        Some("md") => "markdown",
        _ => "text",
    }
}

impl Default for ScopeClient {
    fn default() -> Self {
        Self::new()
    }
}
