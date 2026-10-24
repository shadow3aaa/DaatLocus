//! Native page hosting only. No shell, filesystem, account or arbitrary-script commands.
use serde::{Deserialize, Serialize};

pub const MAX_CAPTURE_BYTES: usize = 8 * 1024 * 1024;
pub const CAPTURE_SCRIPT: &str = include_str!("web-capture.js");
pub const BROWSER_CURSOR_SCRIPT: &str = include_str!("browser-cursor.js");
pub const BROWSER_METRICS_SCRIPT: &str = include_str!("browser-metrics.js");
/// Namespace-local, device-wide browser; Actor resource authorization stays separate.
pub const SHARED_BROWSER_PROFILE: &str =
    "29eb97c72b4a60415cce1cd4754a3738488354e71a46fbcd6f6c8111b1d86d35";

/// Owner UI viewport in CSS coordinates. Never accepted as model arguments.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserBounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub scale: f64,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserHitRegion {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub radius: f64,
}
impl BrowserHitRegion {
    pub fn validate(&self) -> bool {
        [self.x, self.y, self.width, self.height, self.radius]
            .iter()
            .all(|value| value.is_finite() && value.abs() <= 1_000_000.0)
            && self.width > 0.0
            && self.height > 0.0
            && self.radius >= 0.0
    }
}
impl BrowserBounds {
    pub fn validate(&self) -> bool {
        [self.x, self.y, self.width, self.height, self.scale]
            .iter()
            .all(|n| n.is_finite())
            && self.x >= 0.0
            && self.y >= 0.0
            && self.width > 0.0
            && self.height > 0.0
            && self.scale > 0.0
            && self.scale <= 8.0
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    Load { url: String },
    Close,
    Browser { command: BrowserCommand },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum BrowserCommand {
    List,
    Open {
        tab: String,
        url: String,
    },
    Observe {
        tab: String,
    },
    Input {
        tab: String,
        state_id: String,
        input: BrowserInput,
    },
    Close {
        tab: String,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BrowserInput {
    Move {
        x: f64,
        y: f64,
    },
    Drag {
        x: f64,
        y: f64,
        to_x: f64,
        to_y: f64,
    },
    Click {
        x: f64,
        y: f64,
        button: String,
        count: u8,
    },
    Scroll {
        x: f64,
        y: f64,
        delta_x: f64,
        delta_y: f64,
    },
    Type {
        text: String,
    },
    Key {
        key: String,
        modifiers: Vec<String>,
    },
    Navigate {
        url: String,
    },
    Back,
    Forward,
    Reload,
}
impl BrowserCommand {
    pub fn tab(&self) -> Option<&str> {
        match self {
            Self::List => None,
            Self::Open { tab, .. }
            | Self::Observe { tab }
            | Self::Input { tab, .. }
            | Self::Close { tab } => Some(tab),
        }
    }
    pub fn validate(&self) -> bool {
        if self
            .tab()
            .is_some_and(|tab| tab.len() != 32 || !tab.bytes().all(|c| c.is_ascii_hexdigit()))
        {
            return false;
        }
        match self {
            Self::Open { url, .. } => !url.is_empty() && url.len() <= 8192,
            Self::Input {
                state_id, input, ..
            } => {
                state_id.len() == 32
                    && state_id.bytes().all(|c| c.is_ascii_hexdigit())
                    && input.validate()
            }
            _ => true,
        }
    }
}
impl BrowserInput {
    pub fn validate(&self) -> bool {
        match self {
            Self::Move { x, y } => [x, y].iter().all(|n| n.is_finite() && **n >= 0.0),
            Self::Drag { x, y, to_x, to_y } => [x, y, to_x, to_y]
                .iter()
                .all(|n| n.is_finite() && **n >= 0.0),
            Self::Click {
                x,
                y,
                button,
                count,
            } => {
                x.is_finite()
                    && y.is_finite()
                    && *x >= 0.0
                    && *y >= 0.0
                    && matches!(button.as_str(), "left" | "right" | "middle")
                    && (1..=2).contains(count)
            }
            Self::Scroll {
                x,
                y,
                delta_x,
                delta_y,
            } => {
                [x, y, delta_x, delta_y].iter().all(|n| n.is_finite())
                    && *x >= 0.0
                    && *y >= 0.0
                    && delta_x.abs() <= 10000.0
                    && delta_y.abs() <= 10000.0
            }
            Self::Type { text } => text.len() <= 32768 && !text.contains('\0'),
            Self::Key { key, modifiers } => {
                valid_key(key)
                    && modifiers
                        .iter()
                        .all(|m| matches!(m.as_str(), "control" | "alt" | "shift" | "meta"))
                    && modifiers.len() <= 4
            }
            Self::Navigate { url } => !url.is_empty() && url.len() <= 8192,
            _ => true,
        }
    }
}
pub fn valid_key(key: &str) -> bool {
    matches!(
        key,
        "Enter"
            | "Tab"
            | "Escape"
            | "Backspace"
            | "Delete"
            | "ArrowUp"
            | "ArrowDown"
            | "ArrowLeft"
            | "ArrowRight"
            | "Home"
            | "End"
            | "PageUp"
            | "PageDown"
            | "Space"
            | "F3"
            | "F5"
            | "+"
            | "-"
            | "="
    ) || (key.len() == 1 && key.bytes().all(|b| b.is_ascii_alphanumeric()))
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserPage {
    pub tab: String,
    pub url: String,
    pub title: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserObservation {
    pub page: BrowserPage,
    pub state_id: String,
    pub width: u32,
    pub height: u32,
    pub png_base64: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserReply {
    pub pages: Vec<BrowserPage>,
    pub observation: Option<BrowserObservation>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub id: String,
    /// A server-derived opaque profile identifier, never a filesystem path.
    pub profile: String,
    pub operation: Operation,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capture {
    pub url: String,
    pub title: String,
    pub html: String,
    pub notices: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reply {
    pub id: String,
    pub capture: Option<Capture>,
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser: Option<BrowserReply>,
}

impl Request {
    pub fn validate(&self) -> bool {
        self.id.len() == 32
            && self.id.bytes().all(|c| c.is_ascii_hexdigit())
            && self.profile.len() == 64
            && self.profile.bytes().all(|c| c.is_ascii_hexdigit())
            && match &self.operation {
                Operation::Close => true,
                Operation::Load { url } => !url.is_empty() && url.len() <= 8192,
                Operation::Browser { command } => command.validate(),
            }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_browser_operations_pass_the_native_wire_contract() {
        for command in [
            BrowserCommand::List,
            BrowserCommand::Open {
                tab: "a".repeat(32),
                url: "https://example.com/".into(),
            },
            BrowserCommand::Observe {
                tab: "a".repeat(32),
            },
            BrowserCommand::Input {
                tab: "a".repeat(32),
                state_id: "b".repeat(32),
                input: BrowserInput::Reload,
            },
            BrowserCommand::Close {
                tab: "a".repeat(32),
            },
        ] {
            let request = Request {
                id: "c".repeat(32),
                profile: SHARED_BROWSER_PROFILE.into(),
                operation: Operation::Browser { command },
            };
            let decoded: Request =
                serde_json::from_slice(&serde_json::to_vec(&request).unwrap()).unwrap();
            assert!(decoded.validate(), "{:?}", decoded.operation);
        }
    }
}
