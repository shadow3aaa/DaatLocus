use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use serde::{Deserialize, Serialize};

use crate::tool_ui::{ActivateWorkflowUiData, CreateWorkflowUiData, DeepRecallUiData};

use super::primitives::Cell;

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActivateWorkflowActivityCell {
    pub workflow_id: String,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CreateWorkflowActivityCell {
    pub workflow_id: String,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeepRecallActivityCell {
    pub memory_count: usize,
}

impl Cell for ActivateWorkflowActivityCell {
    fn render_lines(&self) -> Vec<Line<'static>> {
        vec![Line::from(vec![
            Span::styled(
                "⌘",
                Style::default()
                    .fg(Color::LightBlue)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(
                format!("Activated Workflow: {}", self.workflow_id),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ])]
    }
}

impl Cell for CreateWorkflowActivityCell {
    fn render_lines(&self) -> Vec<Line<'static>> {
        vec![Line::from(vec![
            Span::styled(
                "⌘",
                Style::default()
                    .fg(Color::LightBlue)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(
                format!("Created Workflow: {}", self.workflow_id),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ])]
    }
}

impl Cell for DeepRecallActivityCell {
    fn render_lines(&self) -> Vec<Line<'static>> {
        vec![Line::from(vec![
            Span::styled(
                "⟲",
                Style::default()
                    .fg(Color::LightBlue)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(
                format!("Recalled {} Memories", self.memory_count),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ])]
    }
}

impl From<ActivateWorkflowUiData> for ActivateWorkflowActivityCell {
    fn from(data: ActivateWorkflowUiData) -> Self {
        ActivateWorkflowActivityCell {
            workflow_id: data.workflow_id,
        }
    }
}

impl From<CreateWorkflowUiData> for CreateWorkflowActivityCell {
    fn from(data: CreateWorkflowUiData) -> Self {
        CreateWorkflowActivityCell {
            workflow_id: data.workflow_id,
        }
    }
}

impl From<DeepRecallUiData> for DeepRecallActivityCell {
    fn from(data: DeepRecallUiData) -> Self {
        DeepRecallActivityCell {
            memory_count: data.memory_count,
        }
    }
}
