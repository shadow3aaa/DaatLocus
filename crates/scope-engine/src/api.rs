use schemars::JsonSchema;
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
    /// Relative file path from project root.
    pub path: String,
    /// File content with per-line hash prefix: `line#hash|original_text\n`
    pub content: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SearchMatch {
    pub file: String,
    pub line: usize,
    pub text: String,
    pub selector: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SearchCodeResponse {
    pub matches: Vec<SearchMatch>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SearchCodeRequest {
    pub query: String,
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
}

#[derive(Debug, Clone, Deserialize)]
pub struct EditCodeRequest {
    pub edits: Vec<StructuredEdit>,
}

/// Operation kind for a single structured edit.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum EditOp {
    Replace,
    Append,
    Prepend,
}

/// Content value: string, array of strings, or null.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(untagged)]
pub enum EditContent {
    Lines(Vec<String>),
    Text(String),
}

impl EditContent {
    pub fn into_lines(self) -> Vec<String> {
        match self {
            EditContent::Lines(lines) => lines,
            EditContent::Text(text) => text.lines().map(str::to_string).collect(),
        }
    }
}

/// One structured edit: op + path + line-hash anchors + optional content.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct StructuredEdit {
    /// Relative file path from project root.
    pub path: String,
    pub op: EditOp,
    /// `line#hash` anchor from read_code response.
    pub start: String,
    /// `line#hash` end anchor (required for replace, ignored otherwise).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end: Option<String>,
    /// Replacement/insertion content as string, array, or null.
    /// `null` with `replace` means delete.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<EditContent>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IsResponsibleSourceRequest {
    pub path: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct IsResponsibleSourceResponse {
    pub is_responsible: bool,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extension: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    pub reason: String,
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
    pub notes: String,
}

// ── Propagation types ────────────────────────────────────────

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum PropagationSource {
    Lsp,
    OpenEnded,
}

#[derive(Debug, Clone, Serialize)]
pub struct PropagationResponse {
    pub propagation_results: Vec<PropagationResult>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PropagationResult {
    pub selector: String,
    pub reason: String,
    pub source: PropagationSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lsp_references: Option<Vec<(String, usize, String)>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff_summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_snippet: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_files: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct NextReviewResponse {
    pub review: Option<ReviewEvent>,
    pub reviews: Vec<ReviewEvent>,
    pub returned: usize,
    pub remaining: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct NextReviewRequest {
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Reference {
    pub selector: String,
    pub line: usize,
    pub context: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
pub enum ReviewEvent {
    KnownReferences {
        modified_symbol: String,
        change_summary: String,
        references: Vec<Reference>,
        file_snippet: String,
    },
    InvestigateImpact {
        modified_symbol: String,
        change_summary: String,
        diff_summary: String,
        file_snippet: String,
        project_files: Vec<String>,
    },
}
