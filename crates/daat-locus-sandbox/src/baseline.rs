//! Shared operating-system runtime reads for host I/O and native processes.
//! Private user and application data directories require ordinary scope grants.
use std::{io, path::PathBuf, sync::OnceLock};

pub fn runtime_read_roots() -> io::Result<&'static [PathBuf]> {
    static ROOTS: OnceLock<io::Result<Vec<PathBuf>>> = OnceLock::new();
    ROOTS
        .get_or_init(platform_roots)
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
