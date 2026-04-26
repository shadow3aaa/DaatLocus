use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use serde::{Deserialize, Serialize};

use crate::tool_ui::{PlanStepUiStatus, PlanUiData, glyph};

use super::primitives::Cell;

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanActivityCell {
    pub steps: Vec<PlanStepActivityCell>,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanStepActivityCell {
    pub status: PlanStepDisplayStatus,
    pub text: String,
}

#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum PlanStepDisplayStatus {
    Pending,
    InProgress,
    Completed,
}

impl Cell for PlanActivityCell {
    fn render_lines(&self) -> Vec<Line<'static>> {
        let mut lines = vec![Line::from(vec![
            Span::styled(
                glyph::PLAN,
                Style::default()
                    .fg(Color::LightBlue)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(
                "Plan",
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ])];
        for step in self.steps.iter().take(8) {
            let (marker, marker_style, text_style) = match step.status {
                PlanStepDisplayStatus::InProgress => (
                    "●",
                    Style::default()
                        .fg(Color::LightBlue)
                        .add_modifier(Modifier::BOLD),
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
                PlanStepDisplayStatus::Pending => (
                    "○",
                    Style::default().fg(Color::DarkGray),
                    Style::default().fg(Color::Gray),
                ),
                PlanStepDisplayStatus::Completed => (
                    "●",
                    Style::default()
                        .fg(Color::LightGreen)
                        .add_modifier(Modifier::BOLD),
                    Style::default().fg(Color::LightGreen),
                ),
            };
            lines.push(Line::from(vec![
                Span::raw("   "),
                Span::styled(marker, marker_style),
                Span::raw(" "),
                Span::styled(step.text.clone(), text_style),
            ]));
        }
        lines
    }
}

impl From<PlanUiData> for PlanActivityCell {
    fn from(data: PlanUiData) -> Self {
        PlanActivityCell {
            steps: data
                .steps
                .into_iter()
                .map(|step| PlanStepActivityCell {
                    status: match step.status {
                        PlanStepUiStatus::Pending => PlanStepDisplayStatus::Pending,
                        PlanStepUiStatus::InProgress => PlanStepDisplayStatus::InProgress,
                        PlanStepUiStatus::Completed => PlanStepDisplayStatus::Completed,
                    },
                    text: step.text,
                })
                .collect(),
        }
    }
}
