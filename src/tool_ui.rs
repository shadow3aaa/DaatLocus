use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind")]
pub enum ToolUiEvent {
    Exec(ToolUiData),
    Terminal(TerminalUiData),
    Patch(PatchUiData),
    Telegram(TelegramUiData),
    Work(ToolUiData),
    Device(ToolUiData),
    Error(ToolUiData),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind")]
pub enum ToolCallUiEvent {
    Exec(ToolUiData),
    Terminal(TerminalUiData),
    Patch(PatchUiData),
    Telegram(TelegramUiData),
    Work(ToolUiData),
    Device(ToolUiData),
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
pub enum TerminalUiAction {
    Execute,
    Continue,
    Poll,
    Terminate,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PatchUiData {
    pub title: String,
    pub summary_line: String,
    #[serde(default)]
    pub files: Vec<PatchFileUiData>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PatchFileUiData {
    pub path: String,
    pub operation: String,
    pub added_lines: usize,
    pub removed_lines: usize,
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
pub enum TelegramUiAction {
    ListChats,
    ReadChat,
    SelectChat,
    SendMessage,
    ResolveChat,
}

impl ToolUiEvent {
    pub fn patch(
        title: impl Into<String>,
        summary_line: impl Into<String>,
        files: Vec<PatchFileUiData>,
    ) -> Self {
        Self::Patch(PatchUiData {
            title: title.into(),
            summary_line: summary_line.into(),
            files,
        })
    }

    pub fn telegram(
        action: TelegramUiAction,
        title: impl Into<String>,
        detail_lines: Vec<String>,
        message_lines: Vec<String>,
    ) -> Self {
        Self::Telegram(TelegramUiData {
            action,
            title: title.into(),
            detail_lines,
            message_lines,
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

    pub fn work(title: impl Into<String>, body_lines: Vec<String>) -> Self {
        Self::Work(ToolUiData {
            title: title.into(),
            body_lines,
        })
    }

    pub fn device(title: impl Into<String>, body_lines: Vec<String>) -> Self {
        Self::Device(ToolUiData {
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
    pub fn patch(
        title: impl Into<String>,
        summary_line: impl Into<String>,
        files: Vec<PatchFileUiData>,
    ) -> Self {
        Self::Patch(PatchUiData {
            title: title.into(),
            summary_line: summary_line.into(),
            files,
        })
    }

    pub fn telegram(
        action: TelegramUiAction,
        title: impl Into<String>,
        detail_lines: Vec<String>,
        message_lines: Vec<String>,
    ) -> Self {
        Self::Telegram(TelegramUiData {
            action,
            title: title.into(),
            detail_lines,
            message_lines,
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

    pub fn work(title: impl Into<String>, body_lines: Vec<String>) -> Self {
        Self::Work(ToolUiData {
            title: title.into(),
            body_lines,
        })
    }

    pub fn device(title: impl Into<String>, body_lines: Vec<String>) -> Self {
        Self::Device(ToolUiData {
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
