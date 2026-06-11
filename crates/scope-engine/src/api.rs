use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// ── Domain types ────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenProjectRequest {
    pub project_root: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OpenProjectResponse {
    pub status: String,
    pub project_root: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detected_lsp_language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lsp: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReadCodeRequest {
    #[serde(default, rename = "ref", alias = "handle")]
    pub ref_handle: Option<String>,
    pub path: Option<String>,
    pub start_line: Option<usize>,
    pub line_count: Option<usize>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ReadCodeResponse {
    /// Relative file path from project root.
    pub path: String,
    /// File content with per-line hash prefix: `line#hash|original_text\n`
    pub content: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SearchCodeResponse {
    pub targets: Vec<SearchTarget>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct SearchTarget {
    pub handle: String,
    pub label: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SearchCodeRequest {
    pub query: String,
    pub path: Option<String>,
    pub include: Option<String>,
    pub limit: Option<usize>,
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
    pub protocol_items: Vec<ScopeProtocolItemSchema>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ScopeProtocolItemSchema {
    pub item: String,
    pub syntax: String,
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
