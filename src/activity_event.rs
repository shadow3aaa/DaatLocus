use serde::{Deserialize, Serialize};

pub mod glyph {
    pub const BROWSER: &str = "↗";
    pub const ERROR: &str = "!";
    pub const EXEC: &str = "•";
    pub const PATCH: &str = "∂";
    pub const PLAN: &str = "∷";
    pub const WORKFLOW: &str = "⌘";
}

pub const EXPLORED_STABLE_ID: &str = "explored";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind")]
pub enum ToolCallActivityEvent {
    Exec(TextActivityDescriptor),
    Terminal(TerminalActivityDescriptor),
    Browser(BrowserActivityDescriptor),
    Patch(PatchActivityDescriptor),
    CodingEdit(CodingEditActivityDescriptor),
    Telegram(TelegramActivityDescriptor),
    Plan(PlanActivityDescriptor),
    #[serde(alias = "Finish", alias = "Work")]
    App(TextActivityDescriptor),
    Error(TextActivityDescriptor),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TextActivityDescriptor {
    pub title: String,
    #[serde(default)]
    pub body_lines: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TerminalActivityDescriptor {
    pub action: TerminalActivityAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<TerminalActivityOrigin>,
    pub title: String,
    #[serde(default)]
    pub body_lines: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct BrowserActivityDescriptor {
    pub action: BrowserActivityAction,
    pub title: String,
    #[serde(default)]
    pub body_lines: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ref_count: Option<usize>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebSearchActivityDescriptor {
    pub action: WebSearchActivityAction,
    pub query: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default)]
    pub body_lines: Vec<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WebSearchActivityAction {
    Searching,
    Searched,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CodingOpenProjectActivityDescriptor {
    pub project_root: String,
    #[serde(default)]
    pub detail_lines: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExploredActivityDescriptor {
    pub stable_id: String,
    pub title: String,
    #[serde(default)]
    pub calls: Vec<ExploredCallActivityDescriptor>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExploredCallActivityDescriptor {
    pub tool_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<ExploredCallActivityAction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secondary_target: Option<String>,
    pub summary: String,
    #[serde(default)]
    pub detail_lines: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExploredCallActivityAction {
    Read,
    List,
    Search,
    Run,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CodingEditActivityDescriptor {
    pub stable_id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_app: Option<String>,
    pub selector: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    pub added_lines: usize,
    pub removed_lines: usize,
    pub propagation_count: usize,
    #[serde(default)]
    pub impact_lines: Vec<String>,
    #[serde(default)]
    pub diff_files: Vec<PatchFileActivityDescriptor>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CodingReviewActivityDescriptor {
    pub title: String,
    pub summary: String,
    pub review_pending: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum TerminalActivityAction {
    Execute,
    Continue,
    Poll,
    Terminate,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TerminalActivityOrigin {
    Agent,
    User,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BrowserActivityAction {
    OpenPage,
    Snapshot,
    Wait,
    Click,
    Fill,
    Back,
    Forward,
    Reload,
    ClosePage,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PatchActivityDescriptor {
    pub summary_line: String,
    #[serde(default)]
    pub files: Vec<PatchFileActivityDescriptor>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PatchFileActivityDescriptor {
    pub path: String,
    pub operation: PatchFileOperation,
    pub added_lines: usize,
    pub removed_lines: usize,
    #[serde(default)]
    pub diff_lines: Vec<PatchDiffLineActivityDescriptor>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PatchFileOperation {
    Add,
    Delete,
    Update,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PatchDiffLineActivityDescriptor {
    pub kind: PatchDiffLineKind,
    pub old_lineno: Option<usize>,
    pub new_lineno: Option<usize>,
    pub text: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PatchDiffLineKind {
    Context,
    Delete,
    Add,
    HunkBreak,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TelegramActivityDescriptor {
    pub action: TelegramActivityAction,
    pub title: String,
    #[serde(default)]
    pub detail_lines: Vec<String>,
    #[serde(default)]
    pub message_lines: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReplyActivityDescriptor {
    pub disposition: ReplyDisposition,
    #[serde(default)]
    pub subject: ReplySubject,
    #[serde(default)]
    pub message_lines: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanActivityDescriptor {
    #[serde(default, rename = "plan_kind", alias = "kind")]
    pub kind: PlanActivityKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explanation: Option<String>,
    pub steps: Vec<PlanStepActivityDescriptor>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum PlanActivityKind {
    Proposed,
    #[default]
    Updated,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanStepActivityDescriptor {
    pub status: PlanStepActivityStatus,
    pub text: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanStepActivityStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReplyDisposition {
    Resolved,
    Dismissed,
    Failed,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReplySubject {
    #[default]
    Message,
    Notice,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum TelegramActivityAction {
    ListChats,
    #[serde(alias = "ReadChat")]
    ReadHistory,
    SelectChat,
    SendMessage,
    ResolveChat,
}

impl ToolCallActivityEvent {
    pub fn coding_edit(data: CodingEditActivityDescriptor) -> Self {
        Self::CodingEdit(data)
    }

    pub fn terminal(
        action: TerminalActivityAction,
        title: impl Into<String>,
        body_lines: Vec<String>,
    ) -> Self {
        Self::Terminal(TerminalActivityDescriptor {
            action,
            origin: None,
            title: title.into(),
            body_lines,
        })
    }

    pub fn plan(data: PlanActivityDescriptor) -> Self {
        Self::Plan(data)
    }

    pub fn app(title: impl Into<String>, body_lines: Vec<String>) -> Self {
        Self::App(TextActivityDescriptor {
            title: title.into(),
            body_lines,
        })
    }

    pub fn error(title: impl Into<String>, body_lines: Vec<String>) -> Self {
        Self::Error(TextActivityDescriptor {
            title: title.into(),
            body_lines,
        })
    }
}

pub fn compact_body_lines(text: &str, max_lines: usize) -> Vec<String> {
    text.lines()
        .map(|line| line.trim_end())
        .filter(|line| !line.is_empty())
        .take(max_lines)
        .map(ToString::to_string)
        .collect()
}

pub fn compact_preserved_body_lines(text: &str, max_lines: usize) -> Vec<String> {
    if max_lines == 0 {
        return Vec::new();
    }

    let mut lines = Vec::new();
    for line in text.lines() {
        let line = line.trim_end();
        if line.trim().is_empty() {
            if !lines.is_empty() {
                lines.push(String::new());
            }
        } else {
            lines.push(line.to_string());
        }

        if lines.len() >= max_lines {
            break;
        }
    }

    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }

    lines
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn sample_plan() -> PlanActivityDescriptor {
        PlanActivityDescriptor {
            kind: PlanActivityKind::Proposed,
            explanation: Some("check the route".to_string()),
            steps: vec![PlanStepActivityDescriptor {
                status: PlanStepActivityStatus::InProgress,
                text: "inspect code".to_string(),
            }],
        }
    }

    #[test]
    fn compact_preserved_body_lines_keeps_diagnostic_layout() {
        let lines = compact_preserved_body_lines(
            "error header\n  1 | import type { TFunction }\n    | ^\n\nnext cause\n",
            8,
        );

        assert_eq!(
            lines,
            vec![
                "error header".to_string(),
                "  1 | import type { TFunction }".to_string(),
                "    | ^".to_string(),
                String::new(),
                "next cause".to_string(),
            ]
        );
    }

    #[test]
    fn tool_call_activity_plan_uses_distinct_wire_field_for_plan_kind() {
        let event = ToolCallActivityEvent::Plan(sample_plan());
        let encoded = serde_json::to_string(&event).unwrap();
        let value = serde_json::to_value(event).unwrap();

        assert_eq!(encoded.matches("\"kind\"").count(), 1);
        assert!(encoded.contains("\"plan_kind\""));
        assert_eq!(value["kind"], json!("Plan"));
        assert_eq!(value["plan_kind"], json!("proposed"));

        let decoded: ToolCallActivityEvent = serde_json::from_value(value).unwrap();
        assert!(matches!(
            decoded,
            ToolCallActivityEvent::Plan(PlanActivityDescriptor {
                kind: PlanActivityKind::Proposed,
                ..
            })
        ));
    }

    #[test]
    fn plan_ui_data_accepts_legacy_standalone_kind_field() {
        let decoded: PlanActivityDescriptor = serde_json::from_value(json!({
            "kind": "proposed",
            "steps": []
        }))
        .unwrap();

        assert_eq!(decoded.kind, PlanActivityKind::Proposed);
    }
}
