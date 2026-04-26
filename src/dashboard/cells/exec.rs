use std::time::Duration;

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use serde::{Deserialize, Serialize};

use crate::tool_ui::{TerminalUiData, ToolUiData, glyph};

use super::primitives::Cell;

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecResultActivityCell {
    pub title: String,
    pub meta: Option<String>,
    pub output_lines: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LiveExecActivityCell {
    pub title: String,
    pub call_lines: Vec<String>,
    pub meta: Option<String>,
    pub output_lines: Vec<String>,
    pub started_at_ms: Option<i64>,
}

impl Cell for ExecResultActivityCell {
    fn render_lines(&self) -> Vec<Line<'static>> {
        let exit_code = self.meta.as_deref().and_then(parse_exit_code_from_meta);
        let indicator_style = if exit_code == Some(0) {
            Style::default()
                .fg(Color::LightGreen)
                .add_modifier(Modifier::BOLD)
        } else if exit_code.is_some() {
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD)
        };
        let mut lines = vec![Line::from(vec![
            Span::styled(glyph::EXEC, indicator_style),
            Span::raw("  "),
            Span::styled(
                "Ran",
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(self.title.clone(), Style::default().fg(Color::White)),
        ])];
        let rendered_output = if self.output_lines.is_empty() {
            vec!["(no output)".to_string()]
        } else {
            truncate_lines_middle(&self.output_lines, 4, 4)
        };
        for line in rendered_output {
            let style = if line.starts_with("… +") {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default().fg(Color::Gray)
            };
            lines.push(Line::from(vec![
                Span::styled("  ", Style::default().fg(Color::DarkGray)),
                Span::styled(line, style),
            ]));
        }
        lines
    }
}

impl Cell for LiveExecActivityCell {
    fn render_lines(&self) -> Vec<Line<'static>> {
        let elapsed = self.started_at_ms.and_then(|started_at_ms| {
            let now_ms = current_time_ms();
            if now_ms >= started_at_ms {
                Some(Duration::from_millis((now_ms - started_at_ms) as u64))
            } else {
                None
            }
        });
        let mut lines = vec![Line::from(vec![
            Span::styled(
                exec_spinner(elapsed),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(
                "Running",
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(self.title.clone(), Style::default().fg(Color::White)),
        ])];
        if self.output_lines.is_empty() {
            lines.push(Line::from(vec![
                Span::styled("  ", Style::default().fg(Color::DarkGray)),
                Span::styled("running...", Style::default().fg(Color::DarkGray)),
            ]));
        }
        let rendered_output = truncate_lines_middle(&self.output_lines, 4, 4);
        for line in rendered_output {
            let style = if line.starts_with("… +") {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default().fg(Color::Gray)
            };
            lines.push(Line::from(vec![
                Span::styled("  ", Style::default().fg(Color::DarkGray)),
                Span::styled(line, style),
            ]));
        }
        lines
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

fn current_time_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn exec_spinner(elapsed: Option<Duration>) -> String {
    const FRAMES: &[&str] = &[glyph::EXEC, "◦", "▪", "◦"];
    let index = elapsed
        .map(|duration| ((duration.as_millis() / 200) as usize) % FRAMES.len())
        .unwrap_or(0);
    FRAMES[index].to_string()
}

fn parse_exit_code_from_meta(meta: &str) -> Option<i32> {
    let exit = meta
        .split_whitespace()
        .find_map(|part| part.strip_prefix("exit="))?;
    exit.parse::<i32>().ok()
}

fn truncate_lines_middle(lines: &[String], head: usize, tail: usize) -> Vec<String> {
    if lines.len() <= head + tail + 1 {
        return lines.to_vec();
    }
    let omitted = lines.len().saturating_sub(head + tail);
    let mut result = Vec::new();
    result.extend(lines.iter().take(head).cloned());
    result.push(format!("… +{omitted} more line(s)"));
    result.extend(lines.iter().skip(lines.len().saturating_sub(tail)).cloned());
    result
}
