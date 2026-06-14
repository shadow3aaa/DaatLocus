use serde::{Deserialize, Serialize};

use crate::tool_ui::{BrowserUiAction, BrowserUiData, WebSearchUiAction, WebSearchUiData};

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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebSearchActivityCell {
    pub action: WebSearchUiAction,
    pub query: String,
    pub url: Option<String>,
    pub body_lines: Vec<String>,
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

impl From<WebSearchUiData> for WebSearchActivityCell {
    fn from(data: WebSearchUiData) -> Self {
        Self {
            action: data.action,
            query: data.query,
            url: data.url,
            body_lines: data.body_lines,
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
