use serde::{Deserialize, Serialize};

use crate::tool_ui::{TerminalUiData, ToolUiData};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecResultActivityCell {
    pub title: String,
    pub meta: Option<String>,
    pub output_lines: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LiveExecActivityCell {
    pub title: String,
    pub call_lines: Vec<String>,
    pub meta: Option<String>,
    pub output_lines: Vec<String>,
    pub started_at_ms: Option<i64>,
}

impl From<ToolUiData> for ExecResultActivityCell {
    fn from(data: ToolUiData) -> Self {
        let mut body_lines = data.body_lines;
        let meta = if body_lines.is_empty() {
            None
        } else {
            Some(body_lines.remove(0))
        };
        ExecResultActivityCell {
            title: data.title,
            meta,
            output_lines: body_lines,
        }
    }
}

impl From<TerminalUiData> for ExecResultActivityCell {
    fn from(data: TerminalUiData) -> Self {
        let mut body_lines = data.body_lines;
        let meta = if body_lines.is_empty() {
            None
        } else {
            Some(body_lines.remove(0))
        };
        ExecResultActivityCell {
            title: data.title,
            meta,
            output_lines: body_lines,
        }
    }
}

pub fn live_exec_cell(
    title: String,
    call_lines: Vec<String>,
    started_at_ms: Option<i64>,
) -> LiveExecActivityCell {
    LiveExecActivityCell {
        title,
        call_lines,
        meta: None,
        output_lines: Vec::new(),
        started_at_ms,
    }
}
