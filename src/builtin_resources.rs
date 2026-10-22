//! Compiled instruction files published into the service's private temporary instance.
use std::path::{Path, PathBuf};

use miette::Result;

use crate::runtime_temp::RuntimeTemp;

/// Trusted bytes and their exact published path. This is not a filesystem grant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuiltinResource {
    pub(crate) path: PathBuf,
    pub(crate) content: &'static [u8],
}

const FILES: &[(&str, &[u8])] = &[
    #[cfg(target_os = "windows")]
    (
        "skills/windows-gui-use/SKILL.md",
        include_bytes!("builtin_skills/windows-gui-use/SKILL.md"),
    ),
    #[cfg(target_os = "macos")]
    (
        "skills/macos-gui-use/SKILL.md",
        include_bytes!("builtin_skills/macos-gui-use/SKILL.md"),
    ),
    #[cfg(target_os = "android")]
    (
        "skills/android-gui-use/SKILL.md",
        include_bytes!("builtin_skills/android-gui-use/SKILL.md"),
    ),
];

pub(crate) fn publish(temporary: &RuntimeTemp) -> Result<Vec<BuiltinResource>> {
    FILES
        .iter()
        .map(|(relative, content)| {
            let path = temporary.publish(Path::new(relative), content)?;
            Ok(BuiltinResource { path, content })
        })
        .collect()
}
