use serde::{Deserialize, Serialize};

use crate::activity_event::{
    BrowserActivityAction, BrowserActivityDescriptor, WebSearchActivityAction,
    WebSearchActivityDescriptor,
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct BrowserActivityData {
    pub title: String,
    pub body_lines: Vec<String>,
    pub url: Option<String>,
    pub line_count: Option<usize>,
    pub ref_count: Option<usize>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LiveBrowserActivityData {
    pub title: String,
    pub body_lines: Vec<String>,
    pub url: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebSearchActivityData {
    pub action: WebSearchActivityAction,
    pub query: String,
    pub url: Option<String>,
    pub body_lines: Vec<String>,
}

impl From<BrowserActivityDescriptor> for BrowserActivityData {
    fn from(data: BrowserActivityDescriptor) -> Self {
        Self {
            title: data.title,
            body_lines: data.body_lines,
            url: data.url,
            line_count: data.line_count,
            ref_count: data.ref_count,
        }
    }
}

impl From<BrowserActivityDescriptor> for LiveBrowserActivityData {
    fn from(data: BrowserActivityDescriptor) -> Self {
        let title = match data.action {
            BrowserActivityAction::OpenPage => data.url.as_deref().map_or_else(
                || "Opening Page".to_string(),
                |url| format!("Opening URL: {}", compact_browser_url(url)),
            ),
            BrowserActivityAction::Wait => "Waiting for Page".to_string(),
            BrowserActivityAction::Click => "Clicking Element".to_string(),
            BrowserActivityAction::Fill => "Filling Element".to_string(),
            BrowserActivityAction::Back => "Going Back".to_string(),
            BrowserActivityAction::Forward => "Going Forward".to_string(),
            BrowserActivityAction::Reload => "Reloading Page".to_string(),
            BrowserActivityAction::ClosePage => "Closing Page".to_string(),
            BrowserActivityAction::Snapshot => "Capturing Page".to_string(),
        };
        Self {
            title,
            body_lines: data.body_lines,
            url: data.url,
        }
    }
}

impl From<WebSearchActivityDescriptor> for WebSearchActivityData {
    fn from(data: WebSearchActivityDescriptor) -> Self {
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
