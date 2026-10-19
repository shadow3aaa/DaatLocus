//! Device-local GUI preferences. These are not Actor configuration or account replication.
#[cfg(windows)]
use miette::{IntoDiagnostic, Result, miette};
use serde::{Deserialize, Serialize};
#[cfg(windows)]
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DisplaySettings {
    pub width: u32,
    pub height: u32,
}
impl Default for DisplaySettings {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Preferences {
    pub enabled: bool,
    pub display: DisplaySettings,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            enabled: true,
            display: DisplaySettings::default(),
        }
    }
}
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Patch {
    pub enabled: Option<bool>,
    pub display: Option<DisplaySettings>,
}
#[cfg(windows)]
impl Preferences {
    pub fn patched(self, patch: Patch) -> Result<Self> {
        let next = Self {
            enabled: patch.enabled.unwrap_or(self.enabled),
            display: patch.display.unwrap_or(self.display),
        };
        next.validate()?;
        Ok(next)
    }
    pub fn validate(self) -> Result<()> {
        if !daat_locus_virtual_workspace::DISPLAY_MODES
            .contains(&(self.display.width, self.display.height))
        {
            return Err(miette!("Unsupported virtual display resolution"));
        }
        Ok(())
    }
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(error).into_diagnostic(),
        };
        let value: Self = serde_json::from_slice(&bytes).into_diagnostic()?;
        value.validate()?;
        Ok(value)
    }
    pub fn save(self, path: &Path) -> Result<()> {
        crate::persistence::write_json_pretty_atomic_sync(
            path,
            &self,
            crate::persistence::PersistenceFileMode::Private,
        )
    }
}
#[cfg(windows)]
impl Patch {
    pub fn validate(&self) -> Result<()> {
        if let Some(display) = self.display {
            Preferences {
                enabled: true,
                display,
            }
            .validate()?;
        }
        Ok(())
    }
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Disabled,
    Ready,
    Unavailable,
    #[cfg(not(windows))]
    Unsupported,
}
#[derive(Serialize)]
pub struct Status {
    pub supported: bool,
    pub enabled: bool,
    pub state: State,
    pub display: DisplaySettings,
    pub modes: Vec<DisplaySettings>,
}
impl Status {
    #[cfg(windows)]
    pub fn new(preferences: Preferences, ready: bool) -> Self {
        Self {
            supported: true,
            enabled: preferences.enabled,
            state: if !preferences.enabled {
                State::Disabled
            } else if ready {
                State::Ready
            } else {
                State::Unavailable
            },
            display: preferences.display,
            modes: daat_locus_virtual_workspace::DISPLAY_MODES
                .iter()
                .map(|&(width, height)| DisplaySettings { width, height })
                .collect(),
        }
    }
    #[cfg(not(windows))]
    pub fn unsupported() -> Self {
        Self {
            supported: false,
            enabled: false,
            state: State::Unsupported,
            display: DisplaySettings::default(),
            modes: vec![],
        }
    }
}
