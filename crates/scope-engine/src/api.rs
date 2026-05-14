use serde::{Deserialize, Serialize};

// ── JSON-RPC 2.0 types ──────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct JsonRpcRequest {
    #[serde(rename = "jsonrpc")]
    pub _jsonrpc: String,
    pub id: serde_json::Value,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: &'static str,
    pub id: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Clone, Serialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
}

impl JsonRpcResponse {
    pub fn ok(id: serde_json::Value, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }
    pub fn err(id: serde_json::Value, code: i64, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
            }),
        }
    }
}

// ── Domain types ────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct OpenProjectRequest {
    pub project_root: String,
    pub language: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReadCodeRequest {
    pub selector: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ReadCodeResponse {
    pub selector: String,
    pub content: String,
    pub language: String,
    pub start_line: usize,
    pub end_line: usize,
    pub selector_info: SelectorInfo,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LineRange {
    pub start_line: usize,
    pub end_line: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SelectorInfo {
    /// Relative file path from project root.
    pub file: String,
    /// Selector kind, such as `symbol`, `line_range`, `around_line`,
    /// `match`, `match_around`, `enclosing_symbol`, or `outline`.
    pub kind: String,
    /// Resolved range when the selector maps to concrete file lines.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<LineRange>,
    /// Canonical symbol selector when the target is or is inside a symbol.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol_selector: Option<String>,
    /// Symbol start line, included whenever known to remove ambiguity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol_start_line: Option<usize>,
    /// Symbol end line, included whenever known to remove ambiguity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol_end_line: Option<usize>,
    /// Definition/name line for the symbol, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub definition_line: Option<usize>,
}

/// Each match from a search_code query.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SearchMatch {
    /// Relative file path from project root.
    pub file: String,
    /// 1-based line number.
    pub line: usize,
    /// Stable match id within a grep/search response.
    pub match_id: String,
    /// The matching line text.
    pub text: String,
    /// The selector of the containing symbol (e.g. "src/net.rs::fn connect()").
    pub selector: Option<String>,
    /// Alias for `selector`, making grep-to-selector bridging explicit.
    pub enclosing_selector: Option<String>,
    /// Structured selector metadata for the match/enclosing symbol.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selector_info: Option<SelectorInfo>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SearchCodeResponse {
    pub matches: Vec<SearchMatch>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SearchCodeRequest {
    pub query: String,
    /// Optional maximum number of matches to return.
    /// If omitted, scope-engine applies a safe default limit.
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GrepCodeRequest {
    pub pattern: String,
    pub path: Option<String>,
    pub include: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GrepCodeResponse {
    pub matches: Vec<SearchMatch>,
    pub output: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GlobFilesRequest {
    pub pattern: String,
    pub path: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GlobFilesResponse {
    pub files: Vec<String>,
    pub truncated: bool,
    pub output: String,
}

/// edit_code parameters: selector + stripped v4a hunk-only patch
#[derive(Debug, Clone, Deserialize)]
pub struct EditCodeRequest {
    pub selector: String,
    /// Stripped v4a patch (hunk-only, no file header)
    pub patch: String,
}

/// delete_code parameters
#[derive(Debug, Clone, Deserialize)]
pub struct DeleteCodeRequest {
    pub selector: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ScopeUsageResponse {
    pub usage_markdown: String,
    pub selector_kinds: Vec<ScopeSelectorKindSchema>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ScopeSelectorKindSchema {
    pub kind: String,
    pub syntax: String,
    pub read: bool,
    pub edit: bool,
    pub delete: bool,
    pub notes: String,
}

// ── Propagation types ────────────────────────────────────────

/// Source of a propagation result.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum PropagationSource {
    /// Cross-file reference found by LSP.
    Lsp,
    /// No LSP available; agent should investigate on its own.
    OpenEnded,
}

#[derive(Debug, Clone, Serialize)]
pub struct PropagationResponse {
    pub propagation_results: Vec<PropagationResult>,
}

/// Result of propagation analysis for a modified symbol.
///
/// When LSP is available, `lsp_references` contains precise cross-file
/// references. When LSP is unavailable, `diff_summary`, `file_snippet`,
/// and `project_files` carry context for the agent to investigate.
#[derive(Debug, Clone, Serialize)]
pub struct PropagationResult {
    pub selector: String,
    pub reason: String,
    pub source: PropagationSource,
    /// LSP references: (selector, line, context) tuples.
    /// Only set when source == Lsp.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lsp_references: Option<Vec<(String, usize, String)>>,
    /// Diff summary of the change.
    /// Only set when source == OpenEnded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff_summary: Option<String>,
    /// Code snippet around the modification site.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_snippet: Option<String>,
    /// Project file list for agent investigation.
    /// Only set when source == OpenEnded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_files: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct NextReviewResponse {
    pub review: Option<ReviewEvent>,
}

/// A reference found by LSP or other precise analysis.
#[derive(Debug, Clone, Serialize)]
pub struct Reference {
    /// Selector of the referencing symbol (e.g. "src/routes.rs::fn login").
    pub selector: String,
    /// 1-based line number of the reference.
    pub line: usize,
    /// Code context around the reference.
    pub context: String,
}

/// A review event produced by SCOPE propagation.
///
/// Two variants based on what the agent should do:
/// - `KnownReferences`: LSP (or other precise tool) found exact cross-file
///   references. Agent should verify each reference is compatible.
/// - `InvestigateImpact`: No precise reference data available. Agent should
///   use search_code and other tools to find and assess impact.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
pub enum ReviewEvent {
    /// References are known precisely. Agent should verify compatibility.
    KnownReferences {
        /// The symbol that was modified.
        modified_symbol: String,
        /// Summary of what changed.
        change_summary: String,
        /// Precise reference locations found by LSP.
        references: Vec<Reference>,
        /// Code snippet around the modification site.
        file_snippet: String,
    },
    /// References are unknown. Agent should investigate impact on its own.
    InvestigateImpact {
        /// The symbol that was modified.
        modified_symbol: String,
        /// Summary of what changed.
        change_summary: String,
        /// The diff hunks describing the change.
        diff_summary: String,
        /// Code snippet around the modification site.
        file_snippet: String,
        /// Project file list to help agent locate potential impact.
        project_files: Vec<String>,
    },
}
