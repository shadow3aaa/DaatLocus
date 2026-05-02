use serde::{Deserialize, Serialize};

use crate::tool_ui::{AppAttentionUiAction, AppAttentionUiData, BrowserUiAction, BrowserUiData};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppAttentionActivityCell {
    pub title: String,
    pub body_lines: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct BrowserActivityCell {
    pub title: String,
    pub body_lines: Vec<String>,
    pub url: Option<String>,
    pub line_count: Option<usize>,
    pub ref_count: Option<usize>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LiveBrowserActivityCell {
    pub title: String,
    pub body_lines: Vec<String>,
    pub url: Option<String>,
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
