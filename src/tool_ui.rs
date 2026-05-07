use serde::{Deserialize, Serialize};

pub mod glyph {
    pub const APP_ATTENTION: &str = "◉";
    pub const BROWSER: &str = "↗";
    pub const ERROR: &str = "!";
    pub const EXEC: &str = "•";
    pub const MEMORY: &str = "⟲";
    pub const PATCH: &str = "∂";
    pub const PLAN: &str = "∷";
    pub const REPLY: &str = "✣";
    pub const TELEGRAM: &str = "◦";
    pub const WORKFLOW: &str = "⌘";
    pub const THINKING: &str = "~";
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind")]
pub enum ToolUiEvent {
    Exec(ToolUiData),
    Terminal(TerminalUiData),
    Browser(BrowserUiData),
    Patch(PatchUiData),
    Telegram(TelegramUiData),
    Reply(ReplyUiData),
    AppAttention(AppAttentionUiData),
    Plan(PlanUiData),
    CreateWorkflow(CreateWorkflowUiData),
    ActivateWorkflow(ActivateWorkflowUiData),
    DeepRecall(DeepRecallUiData),
    #[serde(alias = "Finish", alias = "Work")]
    App(ToolUiData),
    Error(ToolUiData),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind")]
pub enum ToolCallUiEvent {
    Exec(ToolUiData),
    Terminal(TerminalUiData),
    Browser(BrowserUiData),
    Patch(PatchUiData),
    Telegram(TelegramUiData),
    Plan(ToolUiData),
    CreateWorkflow(ToolUiData),
    ActivateWorkflow(ToolUiData),
    DeepRecall(ToolUiData),
    #[serde(alias = "Finish", alias = "Work")]
    App(ToolUiData),
    Error(ToolUiData),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolUiData {
    pub title: String,
    #[serde(default)]
    pub body_lines: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TerminalUiData {
    pub action: TerminalUiAction,
    pub title: String,
    #[serde(default)]
    pub body_lines: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct BrowserUiData {
    pub action: BrowserUiAction,
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
pub enum TerminalUiAction {
    Execute,
    Continue,
    Poll,
    Terminate,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BrowserUiAction {
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
pub struct PatchUiData {
    pub summary_line: String,
    #[serde(default)]
    pub files: Vec<PatchFileUiData>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PatchFileUiData {
    pub path: String,
    pub operation: PatchFileOperation,
    pub added_lines: usize,
    pub removed_lines: usize,
    #[serde(default)]
    pub diff_lines: Vec<PatchDiffLineUiData>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PatchFileOperation {
    Add,
    Delete,
    Update,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PatchDiffLineUiData {
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
pub struct TelegramUiData {
    pub action: TelegramUiAction,
    pub title: String,
    #[serde(default)]
    pub detail_lines: Vec<String>,
    #[serde(default)]
    pub message_lines: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReplyUiData {
    pub disposition: ReplyDisposition,
    #[serde(default)]
    pub subject: ReplySubject,
    #[serde(default)]
    pub message_lines: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppAttentionUiData {
    pub action: AppAttentionUiAction,
    pub app: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanUiData {
    pub steps: Vec<PlanStepUiData>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanStepUiData {
    pub status: PlanStepUiStatus,
    pub text: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanStepUiStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CreateWorkflowUiData {
    pub workflow_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActivateWorkflowUiData {
    pub workflow_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeepRecallUiData {
    pub memory_count: usize,
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
#[serde(rename_all = "snake_case")]
pub enum AppAttentionUiAction {
    Focus,
    PutAway,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum TelegramUiAction {
    ListChats,
    #[serde(alias = "ReadChat")]
    ReadHistory,
    SelectChat,
    SendMessage,
    ResolveChat,
}

impl ToolUiEvent {
    pub fn patch(summary_line: impl Into<String>, files: Vec<PatchFileUiData>) -> Self {
        Self::Patch(PatchUiData {
            summary_line: summary_line.into(),
            files,
        })
    }

    pub fn terminal(
        action: TerminalUiAction,
        title: impl Into<String>,
        body_lines: Vec<String>,
    ) -> Self {
        Self::Terminal(TerminalUiData {
            action,
            title: title.into(),
            body_lines,
        })
    }

    pub fn plan(steps: Vec<PlanStepUiData>) -> Self {
        Self::Plan(PlanUiData { steps })
    }

    pub fn create_workflow(workflow_id: impl Into<String>) -> Self {
        Self::CreateWorkflow(CreateWorkflowUiData {
            workflow_id: workflow_id.into(),
        })
    }

    pub fn activate_workflow(workflow_id: impl Into<String>) -> Self {
        Self::ActivateWorkflow(ActivateWorkflowUiData {
            workflow_id: workflow_id.into(),
        })
    }

    pub fn deep_recall(memory_count: usize) -> Self {
        Self::DeepRecall(DeepRecallUiData { memory_count })
    }

    pub fn reply(disposition: ReplyDisposition, message_lines: Vec<String>) -> Self {
        Self::Reply(ReplyUiData {
            disposition,
            subject: ReplySubject::Message,
            message_lines,
        })
    }

    pub fn notice_reply(disposition: ReplyDisposition, message_lines: Vec<String>) -> Self {
        Self::Reply(ReplyUiData {
            disposition,
            subject: ReplySubject::Notice,
            message_lines,
        })
    }

    pub fn focus_app(app: impl Into<String>) -> Self {
        Self::AppAttention(AppAttentionUiData {
            action: AppAttentionUiAction::Focus,
            app: Some(app.into()),
        })
    }

    pub fn put_away_app() -> Self {
        Self::AppAttention(AppAttentionUiData {
            action: AppAttentionUiAction::PutAway,
            app: None,
        })
    }

    pub fn app(title: impl Into<String>, body_lines: Vec<String>) -> Self {
        Self::App(ToolUiData {
            title: title.into(),
            body_lines,
        })
    }

    pub fn error(title: impl Into<String>, body_lines: Vec<String>) -> Self {
        Self::Error(ToolUiData {
            title: title.into(),
            body_lines,
        })
    }
}

impl ToolCallUiEvent {
    pub fn patch(summary_line: impl Into<String>, files: Vec<PatchFileUiData>) -> Self {
        Self::Patch(PatchUiData {
            summary_line: summary_line.into(),
            files,
        })
    }

    pub fn terminal(
        action: TerminalUiAction,
        title: impl Into<String>,
        body_lines: Vec<String>,
    ) -> Self {
        Self::Terminal(TerminalUiData {
            action,
            title: title.into(),
            body_lines,
        })
    }

    pub fn plan(title: impl Into<String>, body_lines: Vec<String>) -> Self {
        Self::Plan(ToolUiData {
            title: title.into(),
            body_lines,
        })
    }

    pub fn create_workflow(title: impl Into<String>, body_lines: Vec<String>) -> Self {
        Self::CreateWorkflow(ToolUiData {
            title: title.into(),
            body_lines,
        })
    }

    pub fn activate_workflow(title: impl Into<String>, body_lines: Vec<String>) -> Self {
        Self::ActivateWorkflow(ToolUiData {
            title: title.into(),
            body_lines,
        })
    }

    pub fn deep_recall(title: impl Into<String>, body_lines: Vec<String>) -> Self {
        Self::DeepRecall(ToolUiData {
            title: title.into(),
            body_lines,
        })
    }

    pub fn app(title: impl Into<String>, body_lines: Vec<String>) -> Self {
        Self::App(ToolUiData {
            title: title.into(),
            body_lines,
        })
    }

    pub fn error(title: impl Into<String>, body_lines: Vec<String>) -> Self {
        Self::Error(ToolUiData {
            title: title.into(),
            body_lines,
        })
    }
}

pub fn compact_body_lines(text: &str, max_lines: usize) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(max_lines)
        .map(ToString::to_string)
        .collect()
}
