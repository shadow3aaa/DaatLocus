use ratatui::{style::Color, text::Line};
use serde::{Deserialize, Serialize};

use crate::tool_ui::{ToolUiData, glyph};

use super::primitives::{
    Cell, render_error_lines, render_text_activity_lines, render_wait_activity_lines,
};

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AssistantActivityCell {
    pub title: String,
    pub body_lines: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UserActivityCell {
    pub title: String,
    pub body_lines: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GenericAppActivityCell {
    pub title: String,
    pub body_lines: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TerminalWaitActivityCell {
    pub title: String,
    pub body_lines: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ErrorActivityCell {
    pub title: String,
    pub body_lines: Vec<String>,
}

impl Cell for AssistantActivityCell {
    fn render_lines(&self) -> Vec<Line<'static>> {
        render_text_activity_lines("›", Color::Cyan, &self.title, &self.body_lines, 8, None)
    }
}

impl Cell for UserActivityCell {
    fn render_lines(&self) -> Vec<Line<'static>> {
        render_text_activity_lines(
            glyph::EXEC,
            Color::Green,
            &self.title,
            &self.body_lines,
            6,
            None,
        )
    }
}

impl Cell for GenericAppActivityCell {
    fn render_lines(&self) -> Vec<Line<'static>> {
        render_text_activity_lines(
            glyph::EXEC,
            Color::LightGreen,
            &format!("App: {}", self.title),
            &[],
            0,
            None,
        )
    }
}

impl Cell for TerminalWaitActivityCell {
    fn render_lines(&self) -> Vec<Line<'static>> {
        render_wait_activity_lines(&self.title, &self.body_lines, 6)
    }
}

impl Cell for ErrorActivityCell {
    fn render_lines(&self) -> Vec<Line<'static>> {
        render_error_lines(&self.title, &self.body_lines, 12)
    }
}

pub fn assistant_cell(title: impl Into<String>, body_lines: Vec<String>) -> AssistantActivityCell {
    AssistantActivityCell {
        title: title.into(),
        body_lines,
    }
}

pub fn user_cell(title: impl Into<String>, body_lines: Vec<String>) -> UserActivityCell {
    UserActivityCell {
        title: title.into(),
        body_lines,
    }
}

pub fn generic_app_cell(
    title: impl Into<String>,
    body_lines: Vec<String>,
) -> GenericAppActivityCell {
    GenericAppActivityCell {
        title: title.into(),
        body_lines,
    }
}

pub fn terminal_wait_cell(
    title: impl Into<String>,
    body_lines: Vec<String>,
) -> TerminalWaitActivityCell {
    TerminalWaitActivityCell {
        title: title.into(),
        body_lines,
    }
}

pub fn error_cell(title: impl Into<String>, body_lines: Vec<String>) -> ErrorActivityCell {
    ErrorActivityCell {
        title: title.into(),
        body_lines,
    }
}

impl From<ToolUiData> for GenericAppActivityCell {
    fn from(data: ToolUiData) -> Self {
        generic_app_cell(data.title, data.body_lines)
    }
}

impl From<ToolUiData> for ErrorActivityCell {
    fn from(data: ToolUiData) -> Self {
        error_cell(data.title, data.body_lines)
    }
}
