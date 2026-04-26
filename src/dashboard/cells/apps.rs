use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use serde::{Deserialize, Serialize};

use crate::tool_ui::{
    AppAttentionUiAction, AppAttentionUiData, BrowserUiAction, BrowserUiData, glyph,
};

use super::primitives::{Cell, render_text_activity_lines};

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppAttentionActivityCell {
    pub title: String,
    pub body_lines: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BrowserActivityCell {
    pub title: String,
    pub body_lines: Vec<String>,
    pub url: Option<String>,
    pub line_count: Option<usize>,
    pub ref_count: Option<usize>,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LiveBrowserActivityCell {
    pub title: String,
    pub body_lines: Vec<String>,
    pub url: Option<String>,
}

impl Cell for AppAttentionActivityCell {
    fn render_lines(&self) -> Vec<Line<'static>> {
        render_text_activity_lines(
            glyph::APP_ATTENTION,
            Color::LightBlue,
            &self.title,
            &self.body_lines,
            6,
            None,
        )
    }
}

impl Cell for BrowserActivityCell {
    fn render_lines(&self) -> Vec<Line<'static>> {
        let mut lines = vec![Line::from(vec![
            Span::styled(
                glyph::BROWSER,
                Style::default()
                    .fg(Color::LightBlue)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(
                format!(
                    "Captured URL: {}",
                    self.url
                        .as_deref()
                        .map(compact_browser_url)
                        .unwrap_or_else(|| "unknown".to_string())
                ),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ])];
        let mut stats = Vec::new();
        if let Some(line_count) = self.line_count {
            stats.push(format!("{line_count} lines"));
        }
        if let Some(ref_count) = self.ref_count {
            stats.push(format!("{ref_count} refs"));
        }
        if !stats.is_empty() {
            lines.push(Line::from(vec![
                Span::raw("   "),
                Span::styled(stats.join(" · "), Style::default().fg(Color::Gray)),
            ]));
        }
        lines
    }
}

impl Cell for LiveBrowserActivityCell {
    fn render_lines(&self) -> Vec<Line<'static>> {
        let title = self
            .url
            .as_deref()
            .map(|url| format!("Opening URL: {}", compact_browser_url(url)))
            .unwrap_or_else(|| self.title.clone());
        let mut lines = vec![Line::from(vec![
            Span::styled(
                glyph::BROWSER,
                Style::default()
                    .fg(Color::LightBlue)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(
                title,
                Style::default()
                    .fg(Color::LightBlue)
                    .add_modifier(Modifier::BOLD),
            ),
        ])];
        for line in self.body_lines.iter().take(1) {
            lines.push(Line::from(vec![
                Span::raw("   "),
                Span::styled(line.clone(), Style::default().fg(Color::Gray)),
            ]));
        }
        lines
    }
}

impl From<AppAttentionUiData> for AppAttentionActivityCell {
    fn from(data: AppAttentionUiData) -> Self {
        match data.action {
            AppAttentionUiAction::Focus => {
                let app = data.app.unwrap_or_else(|| "app".to_string());
                Self {
                    title: format!("Focused App: {app}"),
                    body_lines: Vec::new(),
                }
            }
            AppAttentionUiAction::PutAway => Self {
                title: "put away focused app".to_string(),
                body_lines: Vec::new(),
            },
        }
    }
}

impl From<BrowserUiData> for BrowserActivityCell {
    fn from(data: BrowserUiData) -> Self {
        Self {
            title: data.title,
            body_lines: data.body_lines,
            url: data.url,
            line_count: data.line_count,
            ref_count: data.ref_count,
        }
    }
}

impl From<BrowserUiData> for LiveBrowserActivityCell {
    fn from(data: BrowserUiData) -> Self {
        let title = match data.action {
            BrowserUiAction::OpenPage => data
                .url
                .as_deref()
                .map(|url| format!("Opening URL: {}", compact_browser_url(url)))
                .unwrap_or_else(|| "Opening Page".to_string()),
            BrowserUiAction::Wait => "Waiting for Page".to_string(),
            BrowserUiAction::Click => "Clicking Element".to_string(),
            BrowserUiAction::Fill => "Filling Element".to_string(),
            BrowserUiAction::Back => "Going Back".to_string(),
            BrowserUiAction::Forward => "Going Forward".to_string(),
            BrowserUiAction::Reload => "Reloading Page".to_string(),
            BrowserUiAction::ClosePage => "Closing Page".to_string(),
            BrowserUiAction::Snapshot => "Capturing Page".to_string(),
        };
        Self {
            title,
            body_lines: data.body_lines,
            url: data.url,
        }
    }
}

fn compact_browser_url(url: &str) -> String {
    const MAX_CHARS: usize = 88;
    let compact = url.trim().replace('\n', "");
    let mut chars = compact.chars();
    let head = chars.by_ref().take(MAX_CHARS).collect::<String>();
    if chars.next().is_some() {
        format!("{head}...")
    } else {
        head
    }
}
