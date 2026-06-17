use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};

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
#[serde(deny_unknown_fields)]
pub struct ReadCodeRequest {
    pub path: String,
    pub anchor: String,
    pub mode: ReadCodeMode,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ReadCodeMode {
    Around,
    Full,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ReadCodeResponse {
    /// File content with per-line hash prefix: `line#hash|original_text\n`
    pub content: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SearchCodeResponse {
    pub matches: Vec<SearchHit>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct SearchHit {
    pub path: String,
    pub hit: String,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SearchMode {
    #[default]
    Literal,
    Regex,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SearchCase {
    Sensitive,
    Insensitive,
    #[default]
    Smart,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct SearchCodeRequest {
    pub query: String,
    #[serde(default)]
    pub mode: SearchMode,
    pub path: Option<String>,
    #[serde(default, deserialize_with = "deserialize_string_list")]
    pub include: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_string_list")]
    pub exclude: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_string_list")]
    pub types: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_string_list")]
    pub type_not: Vec<String>,
    #[serde(default, rename = "case")]
    pub case_mode: SearchCase,
    #[serde(default)]
    pub word: bool,
    #[serde(default)]
    pub line: bool,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default = "default_respect_ignore")]
    pub respect_ignore: bool,
    #[serde(default)]
    pub follow: bool,
    pub limit: Option<usize>,
}

impl Default for SearchCodeRequest {
    fn default() -> Self {
        Self {
            query: String::new(),
            mode: SearchMode::default(),
            path: None,
            include: Vec::new(),
            exclude: Vec::new(),
            types: Vec::new(),
            type_not: Vec::new(),
            case_mode: SearchCase::default(),
            word: false,
            line: false,
            hidden: false,
            respect_ignore: true,
            follow: false,
            limit: None,
        }
    }
}

fn default_respect_ignore() -> bool {
    true
}

fn deserialize_string_list<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringList {
        One(String),
        Many(Vec<String>),
    }

    let Some(value) = Option::<StringList>::deserialize(deserializer)? else {
        return Ok(Vec::new());
    };
    Ok(match value {
        StringList::One(value) => vec![value],
        StringList::Many(values) => values,
    })
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
    pub applied_summary: AppliedStructuredEditSummary,
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

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppliedStructuredEditOperation {
    Add,
    Update,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct AppliedStructuredEditFile {
    pub path: String,
    pub operation: AppliedStructuredEditOperation,
    pub added_lines: usize,
    pub removed_lines: usize,
    pub original_content: String,
    pub new_content: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct AppliedStructuredEditSummary {
    pub files: Vec<AppliedStructuredEditFile>,
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
