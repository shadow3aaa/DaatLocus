//! Shared operating-system runtime reads for host I/O and native processes.
//! These minimum roots also support explicitly read-restricted low-level policies;
//! the account's default full-disk reads do not depend on this baseline.
use std::{
    io,
    path::{Path, PathBuf},
    sync::OnceLock,
};

#[derive(Clone, Debug, serde::Serialize)]
pub struct RuntimeReadScope {
    pub path: PathBuf,
    pub recursive: bool,
}
impl RuntimeReadScope {
    pub(crate) fn covers(&self, path: &Path) -> bool {
        if self.recursive {
            super::path_is_or_descends_resolved(path, &self.path)
        } else {
            super::resolve_path_for_check(path) == super::resolve_path_for_check(&self.path)
        }
    }
}

pub fn runtime_read_scopes() -> io::Result<&'static [RuntimeReadScope]> {
    static ROOTS: OnceLock<io::Result<Vec<RuntimeReadScope>>> = OnceLock::new();
    ROOTS
        .get_or_init(|| {
            let scopes = platform_roots()?
                .into_iter()
                .map(|path| RuntimeReadScope {
                    path,
                    recursive: true,
                })
                .collect::<Vec<_>>();
            #[cfg(target_os = "macos")]
            let scopes = {
                let mut scopes = scopes;
                // libignition/dyld opens the root directory while resolving
                // firmlinks. This is an exact directory read, never a subtree.
                scopes.push(RuntimeReadScope {
                    path: PathBuf::from("/"),
                    recursive: false,
                });
                scopes
            };
            Ok(scopes)
        })
        .as_ref()
        .map(Vec::as_slice)
        .map_err(|error| io::Error::new(error.kind(), error.to_string()))
}

#[cfg(windows)]
fn platform_roots() -> io::Result<Vec<PathBuf>> {
    super::windows::platform_roots()
}

#[cfg(target_os = "linux")]
fn platform_roots() -> io::Result<Vec<PathBuf>> {
    Ok([
        "/usr",
        "/bin",
        "/sbin",
        "/lib",
        "/lib64",
        "/etc/ld.so.cache",
        "/etc/ld.so.conf",
        "/etc/ld.so.conf.d",
        "/etc/alternatives",
        "/etc/nsswitch.conf",
        "/etc/passwd",
        "/etc/group",
        "/etc/hosts",
        "/etc/resolv.conf",
        "/etc/localtime",
        "/etc/ssl/certs",
        "/etc/pki/ca-trust",
    ]
    .into_iter()
    .map(PathBuf::from)
    .collect())
}

#[cfg(target_os = "macos")]
fn platform_roots() -> io::Result<Vec<PathBuf>> {
    Ok([
        "/bin",
        "/sbin",
        "/usr/bin",
        "/usr/sbin",
        "/usr/libexec",
        "/usr/lib",
        "/usr/share",
        "/usr/local/lib",
        "/opt/homebrew/lib",
        "/System/Library",
        "/System/iOSSupport/System/Library",
        "/Library/Apple",
        "/Library/Filesystems/NetFSPlugins",
        "/Library/Preferences",
        "/Applications",
        "/private/var/db/DarwinDirectory/local/recordStore.data",
        "/private/var/db/timezone",
        "/private/var/db/eligibilityd/eligibility.plist",
        "/private/etc/passwd",
        "/private/etc/protocols",
        "/private/etc/services",
        "/private/etc/hosts",
        "/private/etc/resolv.conf",
        "/private/etc/localtime",
        "/private/etc/ssl",
        "/private/etc/nsswitch.conf",
    ]
    .into_iter()
    .map(PathBuf::from)
    .collect())
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn platform_roots() -> io::Result<Vec<PathBuf>> {
    Ok(Vec::new())
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    #[test]
    fn reading_the_runtime_root_directory_does_not_authorize_its_descendants() {
        let outside = crate::test_support::scoped_directory();
        let file = outside.path().join("private.txt");
        std::fs::write(&file, "not a runtime dependency").unwrap();
        let mut policy = crate::SandboxPolicy::disabled();
        policy.filesystem.full_disk_read = false;
        policy.filesystem.full_disk_write = false;
        assert!(policy.is_path_readable(std::path::Path::new("/")));
        assert!(!policy.is_path_readable(&file));
        assert!(!policy.is_path_writable(&file));
    }
}
