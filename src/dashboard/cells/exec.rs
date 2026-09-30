use serde::{Deserialize, Serialize};

use crate::activity_event::{
    TerminalActivityAction, TerminalActivityDescriptor, TerminalActivityOrigin,
    TextActivityDescriptor,
};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommandOutput {
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<TerminalActivityAction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<TerminalActivityOrigin>,
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
pub struct ExecResultActivityData {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_action: Option<TerminalActivityAction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_origin: Option<TerminalActivityOrigin>,
    pub meta: Option<String>,
    pub output_lines: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LiveExecActivityData {
    pub title: String,
    pub call_lines: Vec<String>,
    pub meta: Option<String>,
    pub output_lines: Vec<String>,
    pub started_at_ms: Option<i64>,
}

impl ExecResultActivityData {
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

impl LiveExecActivityData {
    pub fn command_output(&self) -> CommandOutput {
        CommandOutput {
            command: self.title.clone(),
            action: Some(TerminalActivityAction::Execute),
            origin: None,
            meta: TerminalExecutionMeta::parse(self.meta.as_deref(), &self.output_lines),
            output_lines: self.output_lines.clone(),
        }
    }
}

impl TerminalExecutionMeta {
    /// Parse a stored terminal protocol line.
    ///
    /// Producers (`terminal_session_meta`, `render_session_state_line`, and
    /// `terminal_output_metadata_lines`) persist this as text, not a structured
    /// status object. One key=value grammar reads the meta line and any output
    /// line that is itself a byte-count protocol record. Free-form output is not
    /// scanned.
    pub fn parse(meta_line: Option<&str>, output_lines: &[String]) -> Self {
        let mut meta = Self::default();
        if let Some(line) = meta_line {
            meta.parse_meta_line(line);
        }
        for line in output_lines {
            if is_output_metadata_line(line) {
                meta.parse_meta_line(line);
            }
        }
        meta
    }

    pub fn is_running(&self) -> bool {
        self.status
            .as_deref()
            .is_some_and(|status| status == "running")
    }

    fn parse_meta_line(&mut self, line: &str) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return;
        }
        self.raw_fields.push(trimmed.to_string());

        let mut positional = Vec::new();
        for token in trimmed.split_whitespace() {
            if let Some((key, value)) = token.split_once('=') {
                self.set_key_value(key, value);
            } else if positional.len() < 2 {
                positional.push(token);
            }
        }

        if self.session_id.is_none()
            && let Some(session) = positional.first()
        {
            self.session_id = Some((*session).to_string());
        }
        if self.status.is_none()
            && let Some(status) = positional.get(1)
        {
            self.status = Some((*status).to_string());
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
            "output_missed_bytes" => self.output_missed_bytes = parse_byte_count(value),
            "output_dropped_bytes" | "dropped" => {
                self.output_dropped_bytes = parse_byte_count(value);
            }
            "output_retained_bytes" => self.output_retained_bytes = parse_byte_count(value),
            "output_buffer_capacity" => self.output_buffer_capacity = parse_byte_count(value),
            "buffer" => parse_buffer_pair(value, self),
            _ => {}
        }
    }
}

impl From<TextActivityDescriptor> for ExecResultActivityData {
    fn from(data: TextActivityDescriptor) -> Self {
        let mut body_lines = data.body_lines;
        let meta = if body_lines.is_empty() {
            None
        } else {
            Some(body_lines.remove(0))
        };
        Self {
            title: data.title,
            terminal_action: None,
            terminal_origin: None,
            meta,
            output_lines: body_lines,
        }
    }
}

pub(super) fn is_output_metadata_line(line: &str) -> bool {
    let mut saw_byte_count_field = false;
    for token in line.split_whitespace() {
        let Some((key, value)) = token.split_once('=') else {
            return false;
        };
        if !matches!(
            key,
            "output_missed_bytes"
                | "output_dropped_bytes"
                | "output_retained_bytes"
                | "output_buffer_capacity"
        ) {
            return false;
        }
        if parse_byte_count(value).is_none() {
            return false;
        }
        saw_byte_count_field = true;
    }
    saw_byte_count_field
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

impl From<TerminalActivityDescriptor> for ExecResultActivityData {
    fn from(data: TerminalActivityDescriptor) -> Self {
        let mut body_lines = data.body_lines;
        let meta = if body_lines.is_empty() {
            None
        } else {
            Some(body_lines.remove(0))
        };
        Self {
            title: data.title,
            terminal_action: Some(data.action),
            terminal_origin: data.origin,
            meta,
            output_lines: body_lines,
        }
    }
}

pub const fn live_exec_cell(
    title: String,
    call_lines: Vec<String>,
    started_at_ms: Option<i64>,
) -> LiveExecActivityData {
    LiveExecActivityData {
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
        let cell = ExecResultActivityData {
            title: "cargo check".to_string(),
            terminal_action: Some(TerminalActivityAction::Execute),
            terminal_origin: Some(TerminalActivityOrigin::Agent),
            meta: Some(
                "main  exited  exit=0  cwd=C:/repo  dropped=12B  buffer=256/1024B".to_string(),
            ),
            output_lines: vec!["Compiling dashboard".to_string()],
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
        assert!(!output.meta.is_running());
    }

    #[test]
    fn parses_terminal_call_meta_into_structured_output() {
        let meta = TerminalExecutionMeta::parse(
            Some("session=new workdir=C:/repo yield_time_ms=500 wait_mode=timeout"),
            &["this output mentions output_dropped_bytes=99 but is not protocol".to_string()],
        );

        assert_eq!(meta.session_id.as_deref(), Some("new"));
        assert_eq!(meta.cwd.as_deref(), Some("C:/repo"));
        assert_eq!(meta.yield_time_ms, Some(500));
        assert_eq!(meta.wait_mode.as_deref(), Some("timeout"));
        assert_eq!(meta.output_dropped_bytes, None);
    }

    #[test]
    fn parses_byte_count_protocol_fields_without_scanning_free_form_output() {
        let meta = TerminalExecutionMeta::parse(
            Some(
                "session=main status=running exit=- cwd=C:/repo output_missed_bytes=0 output_dropped_bytes=4 output_retained_bytes=8 output_buffer_capacity=16",
            ),
            &["output_dropped_bytes=99 dropped=1B buffer=2/3B".to_string()],
        );

        assert!(meta.is_running());
        assert_eq!(meta.exit_code, None);
        assert_eq!(meta.output_missed_bytes, Some(0));
        assert_eq!(meta.output_dropped_bytes, Some(4));
        assert_eq!(meta.output_retained_bytes, Some(8));
        assert_eq!(meta.output_buffer_capacity, Some(16));
        assert!(is_output_metadata_line(
            "output_missed_bytes=0 output_dropped_bytes=4 output_retained_bytes=8 output_buffer_capacity=16"
        ));
        assert!(!is_output_metadata_line(
            "log output_dropped_bytes=4 is not a metadata record"
        ));

        let from_output_record = TerminalExecutionMeta::parse(
            Some("main  exited  exit=0  cwd=C:/repo"),
            &["output_missed_bytes=0 output_dropped_bytes=12 output_retained_bytes=256 output_buffer_capacity=1024".to_string()],
        );
        assert_eq!(from_output_record.output_missed_bytes, Some(0));
        assert_eq!(from_output_record.output_dropped_bytes, Some(12));
        assert_eq!(from_output_record.output_retained_bytes, Some(256));
        assert_eq!(from_output_record.output_buffer_capacity, Some(1024));
    }
}
