pub mod access;
pub(crate) mod scopes;
pub use scopes::{SandboxScope, SandboxScopeRequest};

// Daat Locus composition of the independent sandbox library.

use std::{
    ops::{Deref, DerefMut},
    path::{Path, PathBuf},
};

pub use daat_locus_sandbox::*;

/// Application manifests stay outside native policy and never grant native access.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeSandboxPolicy {
    pub builtin_resources: Vec<crate::builtin_resources::BuiltinResource>,
    pub native: SandboxPolicy,
}

impl RuntimeSandboxPolicy {
    pub const fn disabled() -> Self {
        Self {
            builtin_resources: Vec::new(),
            native: SandboxPolicy::disabled(),
        }
    }

    pub fn resolve_path(path: &Path, base: Option<&Path>) -> PathBuf {
        SandboxPolicy::resolve_path(path, base)
    }
}

impl Deref for RuntimeSandboxPolicy {
    type Target = SandboxPolicy;
    fn deref(&self) -> &Self::Target {
        &self.native
    }
}

impl DerefMut for RuntimeSandboxPolicy {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.native
    }
}

/// Address namespace for the authenticated OS-owner maintenance channel.
#[cfg(unix)]
pub(crate) fn unix_control_directory() -> PathBuf {
    PathBuf::from("/tmp").join(format!("dl-control-{}", unsafe { libc::geteuid() }))
}

#[cfg(all(test, any(target_os = "linux", target_os = "windows")))]
mod integration_tests;
#[cfg(all(
    test,
    any(target_os = "linux", target_os = "macos", target_os = "windows")
))]
pub(crate) mod test_support;

#[cfg(all(
    test,
    any(target_os = "linux", target_os = "macos", target_os = "windows")
))]
mod process_tests;
