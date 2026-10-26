use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalWaitMode {
    /// Return after new output arrives, the process exits, or the yield window expires.
    AnyOutput,
    /// Wait until the yield window expires or the process exits; do not stream intermediate output updates.
    Timeout,
}
