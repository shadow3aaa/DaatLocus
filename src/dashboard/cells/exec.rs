use serde::{Deserialize, Serialize};

use crate::tool_ui::{TerminalUiAction, TerminalUiData, TerminalUiOrigin, ToolUiData};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommandOutput {
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<TerminalUiAction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<TerminalUiOrigin>,
    #[serde(default)]
    pub meta: TerminalExecutionMeta,
    #[serde(default)]
    pub output_lines: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TerminalExecutionMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub yield_time_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_missed_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_dropped_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_retained_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_buffer_capacity: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub raw_fields: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecResultActivityCell {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_action: Option<TerminalUiAction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_origin: Option<TerminalUiOrigin>,
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

impl ExecResultActivityCell {
    pub fn command_output(&self) -> CommandOutput {
        CommandOutput {
            command: self.title.clone(),
            action: self.terminal_action.clone(),
            origin: self.terminal_origin.clone(),
            meta: TerminalExecutionMeta::parse(self.meta.as_deref(), &self.output_lines),
            output_lines: self.output_lines.clone(),
        }
    }
}

impl LiveExecActivityCell {
    pub fn command_output(&self) -> CommandOutput {
        CommandOutput {
            command: self.title.clone(),
            action: Some(TerminalUiAction::Execute),
            origin: None,
            meta: TerminalExecutionMeta::parse(self.meta.as_deref(), &self.output_lines),
            output_lines: self.output_lines.clone(),
        }
    }
}

impl TerminalExecutionMeta {
    pub fn parse(meta_line: Option<&str>, output_lines: &[String]) -> Self {
        let mut meta = TerminalExecutionMeta::default();
        if let Some(line) = meta_line {
            meta.parse_meta_line(line);
        }
        for line in output_lines {
            if is_output_metadata_line(line) {
                meta.parse_key_value_tokens(line);
            }
        }
        meta
    }

    pub fn is_running(&self) -> bool {
        self.status
            .as_deref()
            .is_some_and(|status| matches!(status, "running" | "status=running"))
    }

    fn parse_meta_line(&mut self, line: &str) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return;
        }

        let mut positional = Vec::new();
        for segment in trimmed.split("  ").map(str::trim).filter(|s| !s.is_empty()) {
            if segment.contains('=') {
                self.parse_key_value_tokens(segment);
            } else {
                positional.push(segment.to_string());
            }
        }

        if positional.len() >= 2 {
            self.session_id.get_or_insert_with(|| positional[0].clone());
            self.status.get_or_insert_with(|| positional[1].clone());
        } else {
            self.parse_key_value_tokens(trimmed);
        }

        if self.raw_fields.is_empty() {
            self.raw_fields.push(trimmed.to_string());
        }
    }

    fn parse_key_value_tokens(&mut self, text: &str) {
        for part in text.split_whitespace() {
            let Some((key, value)) = part.split_once('=') else {
                continue;
            };
            self.set_key_value(key, value);
        }
    }

    fn set_key_value(&mut self, key: &str, value: &str) {
        let value = value.trim();
        if value.is_empty() || matches!(value, "-" | "none" | "default") {
            return;
        }
        match key {
            "session" | "session_id" => self.session_id = Some(value.to_string()),
            "status" => self.status = Some(value.to_string()),
            "exit" | "exit_code" => self.exit_code = value.parse::<i32>().ok(),
            "cwd" | "workdir" => self.cwd = Some(value.to_string()),
            "wait_mode" => self.wait_mode = Some(value.to_string()),
            "yield_time_ms" => self.yield_time_ms = value.parse::<u64>().ok(),
            "output_missed_bytes" => self.output_missed_bytes = value.parse::<u64>().ok(),
            "output_dropped_bytes" | "dropped" => {
                self.output_dropped_bytes = parse_byte_count(value)
            }
            "output_retained_bytes" => self.output_retained_bytes = value.parse::<u64>().ok(),
            "output_buffer_capacity" => self.output_buffer_capacity = value.parse::<u64>().ok(),
            "buffer" => parse_buffer_pair(value, self),
            _ => {}
        }
    }
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
            terminal_action: None,
            terminal_origin: None,
            meta,
            output_lines: body_lines,
        }
    }
}

pub(super) fn is_output_metadata_line(line: &str) -> bool {
    line.contains("output_missed_bytes=")
        || line.contains("output_dropped_bytes=")
        || line.contains("output_retained_bytes=")
        || line.contains("output_buffer_capacity=")
}

fn parse_byte_count(value: &str) -> Option<u64> {
    value.trim_end_matches('B').trim().parse::<u64>().ok()
}

fn parse_buffer_pair(value: &str, meta: &mut TerminalExecutionMeta) {
    let Some((retained, capacity)) = value.split_once('/') else {
        return;
    };
    meta.output_retained_bytes = parse_byte_count(retained);
    meta.output_buffer_capacity = parse_byte_count(capacity);
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
            terminal_action: Some(data.action),
            terminal_origin: data.origin,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_terminal_result_meta_into_structured_output() {
        let cell = ExecResultActivityCell {
            title: "cargo check".to_string(),
            terminal_action: Some(TerminalUiAction::Execute),
            terminal_origin: Some(TerminalUiOrigin::Agent),
            meta: Some("main  exited  exit=0  cwd=C:/repo".to_string()),
            output_lines: vec![
                "output_missed_bytes=0 output_dropped_bytes=12 output_retained_bytes=256 output_buffer_capacity=1024".to_string(),
            ],
        };

        let output = cell.command_output();

        assert_eq!(output.command, "cargo check");
        assert_eq!(output.meta.session_id.as_deref(), Some("main"));
        assert_eq!(output.meta.status.as_deref(), Some("exited"));
        assert_eq!(output.meta.exit_code, Some(0));
        assert_eq!(output.meta.cwd.as_deref(), Some("C:/repo"));
        assert_eq!(output.meta.output_dropped_bytes, Some(12));
        assert_eq!(output.meta.output_retained_bytes, Some(256));
        assert_eq!(output.meta.output_buffer_capacity, Some(1024));
    }

    #[test]
    fn parses_terminal_call_meta_into_structured_output() {
        let meta = TerminalExecutionMeta::parse(
            Some("session=new workdir=C:/repo yield_time_ms=500 wait_mode=timeout"),
            &[],
        );

        assert_eq!(meta.session_id.as_deref(), Some("new"));
        assert_eq!(meta.cwd.as_deref(), Some("C:/repo"));
        assert_eq!(meta.yield_time_ms, Some(500));
        assert_eq!(meta.wait_mode.as_deref(), Some("timeout"));
    }
}
