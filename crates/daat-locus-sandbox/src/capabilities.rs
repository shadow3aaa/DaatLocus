//! Native backend prerequisites, independent of denial-event collection.
//! Unsupported capabilities and probe/configuration errors are distinct.
//! Availability never authorizes a fallback to unrestricted execution.

use std::io;

#[derive(Debug)]
pub enum NativeSupport {
    Supported,
    Unsupported(String),
}

#[cfg(target_os = "windows")]
pub fn native_support() -> io::Result<NativeSupport> {
    use appcontainer_common::base_container_runner::BaseContainerRunner;
    use learning_mode_windows::SecurityEnvironmentApi;

    let api = match SecurityEnvironmentApi::load() {
        Ok(api) => api,
        Err(error) => return windows_support_error(error),
    };
    match api.supports_deny_paths() {
        Ok(true) => {}
        Ok(false) => {
            return Ok(NativeSupport::Unsupported(
                "Windows MXC/PSEC does not expose native deny-path support".into(),
            ));
        }
        Err(error) => return windows_support_error(error),
    }
    // The pinned SDK's bool erases create/attribute errors, including access
    // denied and malformed specifications. Once the API and support bit exist,
    // false is not sufficient evidence to classify this host as unsupported.
    if !BaseContainerRunner::is_process_security_environment_usable() {
        return Err(io::Error::other(
            "MXC/PSEC advertises deny paths but its create/attach probe failed; the pinned SDK does not expose the error, so this cannot be skipped safely",
        ));
    }
    Ok(NativeSupport::Supported)
}

#[cfg(target_os = "windows")]
fn windows_support_error(
    error: learning_mode_windows::LearningModeError,
) -> io::Result<NativeSupport> {
    use learning_mode_windows::LearningModeError;
    let unsupported = match &error {
        LearningModeError::ApiSetUnavailable { .. } | LearningModeError::ExportMissing { .. } => {
            true
        }
        // E_NOTIMPL, HRESULT_FROM_WIN32(ERROR_NOT_SUPPORTED), and
        // HRESULT_FROM_WIN32(ERROR_CALL_NOT_IMPLEMENTED), not arbitrary failures.
        LearningModeError::HResultCall { code, .. } => {
            matches!(*code as u32, 0x8000_4001 | 0x8007_0032 | 0x8007_0078)
        }
        _ => false,
    };
    if unsupported {
        Ok(NativeSupport::Unsupported(error.to_string()))
    } else {
        Err(io::Error::other(error))
    }
}

#[cfg(target_os = "linux")]
pub fn native_support() -> io::Result<NativeSupport> {
    use std::{os::fd::FromRawFd, path::PathBuf, process::Command};

    if !cfg!(any(target_arch = "x86_64", target_arch = "aarch64")) {
        return Ok(NativeSupport::Unsupported(
            "Linux sandbox has no reviewed syscall ABI for this architecture".into(),
        ));
    }
    let Some(bwrap) = super::linux::find_bwrap_in_path() else {
        // The production resolver deliberately rejects untrusted executables.
        // Only complete absence is a skip; a present but unusable installation,
        // inaccessible path, or dangling link is a configuration failure.
        let candidates = ["/usr/bin/bwrap", "/bin/bwrap", "/usr/local/bin/bwrap"]
            .into_iter()
            .map(PathBuf::from)
            .chain(
                std::env::var_os("PATH")
                    .map(|value| {
                        std::env::split_paths(&value)
                            .map(|path| path.join("bwrap"))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default(),
            );
        for path in candidates {
            match std::fs::symlink_metadata(&path) {
                Ok(_) => {
                    return Err(io::Error::other(format!(
                        "bubblewrap exists at {} but does not satisfy the backend's trusted executable requirements",
                        path.display()
                    )));
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        return Ok(NativeSupport::Unsupported(
            "bubblewrap is not installed in the backend's search locations".into(),
        ));
    };
    let version = Command::new(&bwrap).arg("--version").output()?;
    let help = Command::new(&bwrap).arg("--help").output()?;
    if !version.status.success() || !help.status.success() {
        return Err(io::Error::other(format!(
            "bubblewrap capability command failed: version={version:?}; help={help:?}"
        )));
    }
    let version = std::str::from_utf8(&version.stdout).map_err(io::Error::other)?;
    let help = std::str::from_utf8(&help.stdout).map_err(io::Error::other)?;
    if let NativeSupport::Unsupported(reason) = bwrap_support(version, help)? {
        return Ok(NativeSupport::Unsupported(reason));
    }
    let pidfd = unsafe { libc::syscall(libc::SYS_pidfd_open, std::process::id(), 0) };
    if pidfd < 0 {
        return missing_syscall("pidfd_open", io::Error::last_os_error());
    }
    drop(unsafe { std::fs::File::from_raw_fd(pidfd as i32) });
    #[repr(C)]
    struct OpenHow {
        flags: u64,
        mode: u64,
        resolve: u64,
    }
    let how = OpenHow {
        flags: (libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC) as u64,
        mode: 0,
        resolve: 0x04, // RESOLVE_NO_SYMLINKS, matching the production backend.
    };
    let descriptor = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            libc::AT_FDCWD,
            c"/".as_ptr(),
            &how,
            std::mem::size_of::<OpenHow>(),
        )
    };
    if descriptor < 0 {
        return missing_syscall("openat2", io::Error::last_os_error());
    }
    drop(unsafe { std::fs::File::from_raw_fd(descriptor as i32) });
    // Do not preflight a workload and convert its failure into Unsupported.
    // Namespace/seccomp permission failures and real launch failures must fail.
    Ok(NativeSupport::Supported)
}

#[cfg(target_os = "linux")]
fn missing_syscall(name: &str, error: io::Error) -> io::Result<NativeSupport> {
    if error.raw_os_error() == Some(libc::ENOSYS) {
        Ok(NativeSupport::Unsupported(format!(
            "Linux does not implement required {name} syscall"
        )))
    } else {
        Err(error)
    }
}

#[cfg(target_os = "linux")]
fn bwrap_support(version: &str, help: &str) -> io::Result<NativeSupport> {
    let malformed =
        || io::Error::other(format!("malformed bubblewrap version output: {version:?}"));
    let release = version
        .trim()
        .strip_prefix("bubblewrap ")
        .ok_or_else(malformed)?;
    let release: [u32; 3] = release
        .split('.')
        .map(str::parse::<u32>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| malformed())?
        .try_into()
        .map_err(|_| malformed())?;
    let options = help
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .collect::<Vec<_>>();
    if !help.starts_with("usage:")
        || ![
            "--help",
            "--version",
            "--bind",
            "--ro-bind",
            "--unshare-user",
        ]
        .iter()
        .all(|option| options.contains(option))
    {
        return Err(io::Error::other("malformed bubblewrap help output"));
    }
    for option in [
        "--disable-userns",
        "--seccomp",
        "--info-fd",
        "--block-fd",
        "--perms",
    ] {
        if !options.contains(&option) {
            // 0.8.0 introduced --disable-userns and already has the other
            // required options: https://github.com/containers/bubblewrap/releases/tag/v0.8.0
            // A newer version missing them is inconsistent/truncated output,
            // not evidence of an unsupported host. Backports with all options
            // remain usable even if they report an older release number.
            if release < [0, 8, 0] {
                return Ok(NativeSupport::Unsupported(format!(
                    "{} does not support required {option}",
                    version.trim()
                )));
            }
            return Err(io::Error::other(format!(
                "inconsistent bubblewrap version/help: {} omitted required {option}",
                version.trim()
            )));
        }
    }
    Ok(NativeSupport::Supported)
}

#[cfg(target_os = "macos")]
pub fn native_support() -> io::Result<NativeSupport> {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::symlink_metadata("/usr/bin/sandbox-exec") {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(NativeSupport::Unsupported(
                "macOS sandbox-exec is not installed".into(),
            ));
        }
        Err(error) => return Err(error),
    }
    let metadata = std::fs::metadata("/usr/bin/sandbox-exec")?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
        return Err(io::Error::other(
            "/usr/bin/sandbox-exec is not an executable file",
        ));
    }
    Ok(NativeSupport::Supported)
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    const BASE_HELP: &str = "usage: bwrap [OPTIONS...] COMMAND\n --help Print help\n --version Print version\n --bind SRC DEST\n --ro-bind SRC DEST\n --unshare-user\n";

    #[test]
    fn bubblewrap_probe_distinguishes_old_features_from_broken_output() {
        assert!(matches!(
            bwrap_support("bubblewrap 0.4.0\n", BASE_HELP).unwrap(),
            NativeSupport::Unsupported(_)
        ));
        let complete = format!(
            "{BASE_HELP} --disable-userns\n --seccomp FD\n --info-fd FD\n --block-fd FD\n --perms OCTAL\n"
        );
        assert!(matches!(
            bwrap_support("bubblewrap 0.12.0\n", &complete).unwrap(),
            NativeSupport::Supported
        ));
        assert!(bwrap_support("not a version", &complete).is_err());
        assert!(bwrap_support("bubblewrap 0.12.0", "truncated help").is_err());
        // A valid-looking prefix must not masquerade as an older feature set.
        assert!(bwrap_support("bubblewrap 0.12.0", BASE_HELP).is_err());
        assert!(bwrap_support("bubblewrap 0.8.0", BASE_HELP).is_err());
    }

    #[test]
    fn syscall_permissions_are_not_unsupported_capabilities() {
        assert!(matches!(
            missing_syscall("openat2", io::Error::from_raw_os_error(libc::ENOSYS)).unwrap(),
            NativeSupport::Unsupported(_)
        ));
        for errno in [libc::EPERM, libc::EACCES, libc::EINVAL, libc::EMFILE] {
            assert_eq!(
                missing_syscall("openat2", io::Error::from_raw_os_error(errno))
                    .unwrap_err()
                    .raw_os_error(),
                Some(errno)
            );
        }
    }
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::*;
    use learning_mode_windows::LearningModeError;

    #[test]
    fn windows_probe_does_not_skip_permission_or_invalid_input_errors() {
        for code in [0x8000_4001u32, 0x8007_0032, 0x8007_0078] {
            assert!(matches!(
                windows_support_error(LearningModeError::HResultCall {
                    function: "QueryProcessSecurityEnvironmentSupport",
                    code: code as i32,
                })
                .unwrap(),
                NativeSupport::Unsupported(_)
            ));
        }
        for code in [0x8007_0005u32, 0x8007_0057, 0x8000_4005] {
            assert!(
                windows_support_error(LearningModeError::HResultCall {
                    function: "QueryProcessSecurityEnvironmentSupport",
                    code: code as i32,
                })
                .is_err()
            );
        }
        assert!(windows_support_error(LearningModeError::DllLoad("access denied".into())).is_err());
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
pub fn native_support() -> io::Result<NativeSupport> {
    Ok(NativeSupport::Unsupported(
        "No native process sandbox backend for this platform".into(),
    ))
}
