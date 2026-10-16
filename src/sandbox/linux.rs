use std::{
    env,
    path::{Path, PathBuf},
};

use miette::{Result, miette};

use super::{
    FileSystemSandboxPolicy, RuntimeSandboxPolicy, SandboxSpawnSpec, StrongFilesystemSandboxMode,
    path_is_denied, policy_paths_with_resolved,
};

const BWRAP_PROGRAM: &str = "bwrap";

#[cfg(test)]
pub fn wrap_shell_command(
    policy: &RuntimeSandboxPolicy,
    program: &str,
    args: Vec<String>,
) -> Result<SandboxSpawnSpec> {
    wrap_command(policy, PathBuf::from(program), args)
}

pub fn wrap_command(
    policy: &RuntimeSandboxPolicy,
    program: PathBuf,
    args: Vec<String>,
) -> Result<SandboxSpawnSpec> {
    if policy.strong_filesystem == StrongFilesystemSandboxMode::Off
        || !requires_backend(&policy.filesystem)
    {
        return Ok(unwrapped_spawn_spec(program, args));
    }

    let Some(bwrap) = find_bwrap_in_path() else {
        return match policy.strong_filesystem {
            StrongFilesystemSandboxMode::Auto => {
                tracing::warn!(
                    "Linux strong filesystem sandbox requested in auto mode, but `bwrap` was not found on PATH"
                );
                Ok(unwrapped_spawn_spec(program, args))
            }
            StrongFilesystemSandboxMode::Required => {
                Err(miette!("Linux filesystem sandbox requires `bwrap` on PATH"))
            }
            // Returned above when the mode is Off. Kept so the match is exhaustive.
            StrongFilesystemSandboxMode::Off => Ok(unwrapped_spawn_spec(program, args)),
        };
    };

    let mut command = Vec::with_capacity(args.len() + 1);
    command.push(program.to_string_lossy().into_owned());
    command.extend(args);
    Ok(SandboxSpawnSpec {
        program: bwrap,
        args: create_bwrap_args(&policy.filesystem, command),
    })
}

fn unwrapped_spawn_spec(program: PathBuf, args: Vec<String>) -> SandboxSpawnSpec {
    SandboxSpawnSpec { program, args }
}

fn requires_backend(policy: &FileSystemSandboxPolicy) -> bool {
    !(policy.full_disk_read
        && policy.full_disk_write
        && policy.readable_roots.is_empty()
        && policy.writable_roots.is_empty()
        && policy.deny_read_paths.is_empty()
        && policy.deny_write_paths.is_empty())
}

fn find_bwrap_in_path() -> Option<PathBuf> {
    let search_path = env::var_os("PATH")?;
    let cwd = env::current_dir().ok()?;
    env::split_paths(&search_path)
        .map(|entry| entry.join(BWRAP_PROGRAM))
        .filter_map(|candidate| {
            if !candidate.is_file() {
                return None;
            }
            Some(std::fs::canonicalize(&candidate).unwrap_or(candidate))
        })
        .find(|candidate| !candidate.starts_with(&cwd))
}

fn create_bwrap_args(policy: &FileSystemSandboxPolicy, command: Vec<String>) -> Vec<String> {
    let mut args = vec![
        "--new-session".to_string(),
        "--die-with-parent".to_string(),
        "--unshare-user".to_string(),
        "--unshare-pid".to_string(),
    ];

    push_root_mounts(&mut args, policy);
    push_writable_root_mounts(&mut args, policy);
    // A later host bind (especially a writable `/`) would hide these mounts
    // and restore host devices/procfs inside the new PID namespace.
    args.extend(["--dev", "/dev", "--proc", "/proc"].map(String::from));
    push_read_only_subpath_mounts(&mut args, policy);
    push_deny_write_mounts(&mut args, policy);
    push_deny_read_mounts(&mut args, policy);

    args.push("--".to_string());
    args.extend(command);
    args
}

fn push_root_mounts(args: &mut Vec<String>, policy: &FileSystemSandboxPolicy) {
    if policy.full_disk_write {
        push_mount(args, "--bind", Path::new("/"), Path::new("/"));
    } else if policy.full_disk_read {
        push_mount(args, "--ro-bind", Path::new("/"), Path::new("/"));
    } else {
        args.push("--tmpfs".to_string());
        args.push("/".to_string());
        for root in policy_paths_with_resolved(&policy.readable_roots) {
            push_existing_mount(args, "--ro-bind", &root);
        }
    }
}

fn push_writable_root_mounts(args: &mut Vec<String>, policy: &FileSystemSandboxPolicy) {
    for writable_root in &policy.writable_roots {
        for root in policy_paths_with_resolved(std::slice::from_ref(&writable_root.root)) {
            push_existing_mount(args, "--bind", &root);
        }
    }
}

fn push_read_only_subpath_mounts(args: &mut Vec<String>, policy: &FileSystemSandboxPolicy) {
    for writable_root in &policy.writable_roots {
        for subpath in policy_paths_with_resolved(&writable_root.read_only_subpaths) {
            push_existing_mount(args, "--ro-bind", &subpath);
        }
    }
}

fn push_deny_write_mounts(args: &mut Vec<String>, policy: &FileSystemSandboxPolicy) {
    for path in policy_paths_with_resolved(&policy.deny_write_paths) {
        push_existing_mount(args, "--ro-bind", &path);
    }
}

fn push_deny_read_mounts(args: &mut Vec<String>, policy: &FileSystemSandboxPolicy) {
    let mut read_only_masks = Vec::new();
    for path in policy_paths_with_resolved(&policy.deny_read_paths) {
        if path.is_dir() {
            args.push("--tmpfs".to_string());
            args.push(path_to_arg(&path));
            if path_is_denied(&path, &policy.deny_write_paths) {
                read_only_masks.push(path);
            }
        } else if path.is_file() {
            push_mount(args, "--ro-bind", Path::new("/dev/null"), &path);
        }
    }
    // A parent tmpfs hides the original child mountpoints. Let bwrap create all
    // nested masks before sealing them; sealing a parent alone would also leave
    // its child tmpfs mounts writable.
    for path in read_only_masks.into_iter().rev() {
        args.push("--remount-ro".to_string());
        args.push(path_to_arg(&path));
    }
}

fn push_existing_mount(args: &mut Vec<String>, flag: &str, path: &Path) {
    if path.exists() {
        push_mount(args, flag, path, path);
    }
}

fn push_mount(args: &mut Vec<String>, flag: &str, source: &Path, destination: &Path) {
    args.push(flag.to_string());
    args.push(path_to_arg(source));
    args.push(path_to_arg(destination));
}

fn path_to_arg(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::WritableRoot;

    #[test]
    fn bwrap_args_protect_full_disk_policy_with_denies() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let protected_home = tempdir.path().join("private").join("daat");
        let source_root = tempdir.path().join("workspace").join("source");
        std::fs::create_dir_all(&protected_home).expect("create protected home");
        std::fs::create_dir_all(&source_root).expect("create source root");
        let policy = FileSystemSandboxPolicy {
            full_disk_read: true,
            full_disk_write: true,
            readable_roots: Vec::new(),
            writable_roots: Vec::new(),
            deny_read_paths: vec![protected_home.clone()],
            private_delivery: None,
            bound_roots: Vec::new(),
            deny_write_paths: vec![source_root.clone()],
        };

        let args = create_bwrap_args(&policy, vec!["sh".to_string(), "-lc".to_string()]);

        assert!(contains_triplet(&args, "--bind", "/", "/"));
        assert!(contains_pair(
            &args,
            "--tmpfs",
            protected_home.to_string_lossy().as_ref()
        ));
        assert!(contains_triplet(
            &args,
            "--ro-bind",
            source_root.to_string_lossy().as_ref(),
            source_root.to_string_lossy().as_ref()
        ));
        assert_eq!(args.iter().filter(|arg| *arg == "--").count(), 1);
        assert_eq!(args.last().map(String::as_str), Some("-lc"));
    }

    #[test]
    fn bwrap_args_create_nested_masks_before_sealing_each_directory() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let protected_home = tempdir.path().join(".daat-locus");
        let protected_config = protected_home.join("config");
        let secret = protected_config.join("secret.txt");
        std::fs::create_dir_all(&protected_config).expect("create protected config");
        std::fs::write(&secret, "secret").expect("write secret");
        let policy = FileSystemSandboxPolicy {
            full_disk_read: true,
            full_disk_write: true,
            readable_roots: Vec::new(),
            writable_roots: Vec::new(),
            deny_read_paths: vec![
                protected_config.clone(),
                protected_home.clone(),
                secret.clone(),
            ],
            private_delivery: None,
            bound_roots: Vec::new(),
            deny_write_paths: vec![protected_home.clone()],
        };

        let args = create_bwrap_args(&policy, vec!["sh".to_string()]);

        let first_seal = args.iter().position(|arg| arg == "--remount-ro").unwrap();
        for directory in [&protected_home, &protected_config] {
            let path = path_to_arg(directory);
            let mount = args
                .windows(2)
                .position(|pair| pair == ["--tmpfs", &path])
                .unwrap();
            assert!(
                mount < first_seal,
                "nested mountpoint must exist before sealing"
            );
            assert!(
                contains_pair(&args, "--remount-ro", &path),
                "every nested tmpfs must deny writes"
            );
        }
        let file_mask = args
            .windows(3)
            .position(|triple| triple == ["--ro-bind", "/dev/null", &path_to_arg(&secret)])
            .unwrap();
        assert!(
            file_mask < first_seal,
            "file mountpoint must exist before sealing"
        );
    }

    #[test]
    fn bwrap_args_reopen_writable_roots_under_read_only_root() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let workspace = tempdir.path().join("workspace");
        let git_dir = workspace.join(".git");
        std::fs::create_dir_all(&git_dir).expect("create git dir");
        let policy = FileSystemSandboxPolicy {
            full_disk_read: true,
            full_disk_write: false,
            readable_roots: Vec::new(),
            writable_roots: vec![WritableRoot {
                root: workspace.clone(),
                read_only_subpaths: vec![git_dir.clone()],
            }],
            deny_read_paths: Vec::new(),
            private_delivery: None,
            bound_roots: Vec::new(),
            deny_write_paths: Vec::new(),
        };

        let args = create_bwrap_args(&policy, vec!["sh".to_string()]);

        assert!(contains_triplet(&args, "--ro-bind", "/", "/"));
        assert!(contains_triplet(
            &args,
            "--bind",
            workspace.to_string_lossy().as_ref(),
            workspace.to_string_lossy().as_ref()
        ));
        assert!(contains_triplet(
            &args,
            "--ro-bind",
            git_dir.to_string_lossy().as_ref(),
            git_dir.to_string_lossy().as_ref()
        ));
    }

    #[test]
    fn bwrap_args_keep_private_devices_and_proc_after_writable_root_grants() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let read_only = tempdir.path().join("read-only");
        std::fs::create_dir(&read_only).expect("create read-only directory");
        // A later ancestor grant must neither hide the virtual filesystems nor
        // discard a previous root's read-only exceptions.
        let mut policy = FileSystemSandboxPolicy::unrestricted();
        policy.full_disk_write = false;
        policy.writable_roots = vec![
            WritableRoot {
                root: tempdir.path().to_owned(),
                read_only_subpaths: vec![read_only.clone()],
            },
            WritableRoot {
                root: PathBuf::from("/"),
                read_only_subpaths: vec![],
            },
        ];
        let args = create_bwrap_args(&policy, vec!["sh".to_string()]);
        let root_grant = args
            .windows(3)
            .rposition(|triple| triple == ["--bind", "/", "/"])
            .unwrap();
        let devices = args
            .windows(2)
            .position(|pair| pair == ["--dev", "/dev"])
            .unwrap();
        let proc = args
            .windows(2)
            .position(|pair| pair == ["--proc", "/proc"])
            .unwrap();
        let restriction = args
            .windows(3)
            .position(|triple| {
                triple
                    == [
                        "--ro-bind",
                        &path_to_arg(&read_only),
                        &path_to_arg(&read_only),
                    ]
            })
            .unwrap();
        assert!(
            root_grant < devices,
            "root grant must not hide private devices"
        );
        assert!(root_grant < proc, "root grant must not expose host procfs");
        assert!(
            devices < restriction && proc < restriction,
            "explicit restrictions must remain effective"
        );
    }

    #[tokio::test]
    #[ignore = "requires bwrap and user namespace support"]
    async fn writable_root_preserves_private_devices_and_pid_namespace() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let read_only = tempdir.path().join("read-only");
        let private = tempdir.path().join("private");
        let allowed = tempdir.path().join("allowed.txt");
        std::fs::create_dir(&read_only).unwrap();
        std::fs::create_dir(&private).unwrap();
        std::fs::write(read_only.join("visible.txt"), "readable").unwrap();
        std::fs::write(private.join("secret.txt"), "secret").unwrap();
        let mut filesystem = FileSystemSandboxPolicy::unrestricted();
        filesystem.full_disk_write = false;
        filesystem.writable_roots = vec![
            WritableRoot {
                root: tempdir.path().to_owned(),
                read_only_subpaths: vec![read_only.clone()],
            },
            WritableRoot {
                root: PathBuf::from("/"),
                read_only_subpaths: vec![],
            },
        ];
        filesystem.deny_read_paths = vec![private.clone()];
        filesystem.deny_write_paths = vec![private.clone()];
        let policy = RuntimeSandboxPolicy {
            filesystem,
            protected_env_vars: vec![],
            strong_filesystem: StrongFilesystemSandboxMode::Required,
        };
        let script = r#"set -eu
[ -c /dev/null ]
printf device-probe > /dev/null
cat /dev/null
# The shell builtin opens procfs itself, so its reported PID must equal $$.
# A host procfs bind instead reports the shell's outer namespace PID.
read proc_pid rest < /proc/self/stat
[ "$proc_pid" = "$$" ]
[ "$(cat "$READ_ONLY/visible.txt")" = readable ]
if printf forbidden 2>/dev/null > "$READ_ONLY/created.txt"; then exit 21; fi
if cat "$PRIVATE/secret.txt" >/dev/null 2>&1; then exit 22; fi
if printf forbidden 2>/dev/null > "$PRIVATE/created.txt"; then exit 23; fi
printf ok > "$ALLOWED"
"#;
        let spawn = wrap_shell_command(&policy, "/bin/sh", vec!["-c".into(), script.into()])
            .expect("required bwrap backend must be available");
        let output = tokio::time::timeout(
            std::time::Duration::from_secs(20),
            tokio::process::Command::new(spawn.program)
                .args(spawn.args)
                .current_dir(tempdir.path())
                .env("READ_ONLY", &read_only)
                .env("PRIVATE", &private)
                .env("ALLOWED", &allowed)
                .kill_on_drop(true)
                .output(),
        )
        .await
        .expect("sandbox probe timed out")
        .expect("start sandbox probe");
        assert!(output.status.success(), "sandbox probe failed: {output:?}");
        assert_eq!(std::fs::read_to_string(allowed).unwrap(), "ok");
        assert!(!read_only.join("created.txt").exists());
        assert!(!private.join("created.txt").exists());
        assert_eq!(
            std::fs::read_to_string(private.join("secret.txt")).unwrap(),
            "secret"
        );
    }

    #[test]
    fn no_backend_needed_for_unrestricted_policy() {
        let policy = FileSystemSandboxPolicy {
            full_disk_read: true,
            full_disk_write: true,
            readable_roots: Vec::new(),
            writable_roots: Vec::new(),
            deny_read_paths: Vec::new(),
            private_delivery: None,
            bound_roots: Vec::new(),
            deny_write_paths: Vec::new(),
        };

        assert!(!requires_backend(&policy));
    }

    fn contains_triplet(args: &[String], flag: &str, source: &str, destination: &str) -> bool {
        args.windows(3).any(|item| {
            item[0].as_str() == flag
                && item[1].as_str() == source
                && item[2].as_str() == destination
        })
    }

    fn contains_pair(args: &[String], first: &str, second: &str) -> bool {
        args.windows(2)
            .any(|item| item[0].as_str() == first && item[1].as_str() == second)
    }
}
