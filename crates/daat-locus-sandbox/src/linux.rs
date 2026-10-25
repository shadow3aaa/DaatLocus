#[path = "linux/process.rs"]
mod process;
pub(super) use process::{LinuxSandboxAsyncChild, spawn_restricted_async};

use std::{
    env,
    path::{Path, PathBuf},
};

use miette::{Result, miette};

use super::{
    FileSystemSandboxPolicy, SandboxPolicy, SandboxSpawnSpec, StrongFilesystemSandboxMode,
    policy_paths_with_resolved,
};

const BWRAP_PROGRAM: &str = "bwrap";

#[cfg(test)]
pub fn wrap_shell_command(
    policy: &SandboxPolicy,
    program: &str,
    args: Vec<String>,
) -> Result<SandboxSpawnSpec> {
    wrap_command(policy, PathBuf::from(program), args)
}

pub fn wrap_command(
    policy: &SandboxPolicy,
    program: PathBuf,
    args: Vec<String>,
) -> Result<SandboxSpawnSpec> {
    if policy.strong_filesystem == StrongFilesystemSandboxMode::Off
        || !requires_backend(&policy.filesystem)
    {
        return Ok(unwrapped_spawn_spec(program, args));
    }

    let bwrap = find_bwrap_in_path()
        .ok_or_else(|| miette!("Linux filesystem sandbox requires `bwrap` on PATH"))?;
    validate_paths(&policy.filesystem)?;

    let mut command = Vec::with_capacity(args.len() + 1);
    command.push(program.to_string_lossy().into_owned());
    command.extend(args);
    Ok(SandboxSpawnSpec {
        program: bwrap,
        args: create_bwrap_args(&policy.filesystem, command)?,
    })
}

fn unwrapped_spawn_spec(program: PathBuf, args: Vec<String>) -> SandboxSpawnSpec {
    SandboxSpawnSpec { program, args }
}

pub(super) fn requires_backend(policy: &FileSystemSandboxPolicy) -> bool {
    policy.deny_all_writes
        || !(policy.full_disk_read
            && policy.full_disk_write
            && policy.readable_roots.is_empty()
            && policy.writable_roots.is_empty()
            && policy.deny_read_paths.is_empty()
            && policy.deny_write_paths.is_empty())
}

pub(super) fn find_bwrap_in_path() -> Option<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    // Daemons commonly start in `/`; excluding everything beneath cwd would
    // reject every system executable. Prefer root-managed system locations and
    // accept PATH alternatives only through root-managed, nonwritable parents.
    let search = [
        PathBuf::from("/usr/bin/bwrap"),
        PathBuf::from("/bin/bwrap"),
        PathBuf::from("/usr/local/bin/bwrap"),
    ];
    let extra = env::var_os("PATH")
        .map(|value| {
            env::split_paths(&value)
                .map(|entry| entry.join(BWRAP_PROGRAM))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    search
        .into_iter()
        .chain(extra)
        .filter_map(|path| std::fs::canonicalize(path).ok())
        .find(|path| {
            std::fs::metadata(path).is_ok_and(|md| {
                md.is_file() && md.uid() == 0 && md.mode() & 0o022 == 0 && md.mode() & 0o111 != 0
            }) && path.ancestors().skip(1).all(|parent| {
                std::fs::metadata(parent)
                    .is_ok_and(|md| md.is_dir() && md.uid() == 0 && md.mode() & 0o022 == 0)
            })
        })
}

fn create_bwrap_args(
    policy: &FileSystemSandboxPolicy,
    command: Vec<String>,
) -> Result<Vec<String>> {
    let mut args = vec![
        "--new-session".to_string(),
        "--die-with-parent".to_string(),
        "--unshare-user".to_string(),
        "--unshare-pid".to_string(),
        "--unshare-ipc".to_string(),
        "--unshare-uts".to_string(),
        "--cap-drop".to_string(),
        "ALL".to_string(),
        "--disable-userns".to_string(),
    ];

    push_root_mounts(&mut args, policy)?;
    push_writable_root_mounts(&mut args, policy);
    // A later host bind (especially a writable `/`) would hide these mounts
    // and restore host devices/procfs inside the new PID namespace.
    args.extend(["--dev", "/dev", "--proc", "/proc"].map(String::from));
    push_read_only_subpath_mounts(&mut args, policy);
    push_deny_write_mounts(&mut args, policy);
    push_deny_read_mounts(&mut args, policy);
    if policy.deny_all_writes {
        args.extend(["--remount-ro".to_string(), "/".to_string()]);
        if Path::new("/tmp").exists() {
            args.extend(["--remount-ro".to_string(), "/tmp".to_string()]);
        }
    }

    args.push("--".to_string());
    args.extend(command);
    Ok(args)
}

fn push_root_mounts(args: &mut Vec<String>, policy: &FileSystemSandboxPolicy) -> Result<()> {
    if policy.full_disk_write && !policy.deny_all_writes {
        push_mount(args, "--bind", Path::new("/"), Path::new("/"));
    } else if policy.full_disk_read || policy.full_disk_write {
        push_mount(args, "--ro-bind", Path::new("/"), Path::new("/"));
    } else {
        args.push("--tmpfs".to_string());
        args.push("/".to_string());
        // Establish the mount point before applying the shared temporary roots
        // and explicit grants. Every host path uses the common policy below.
        args.extend(["--tmpfs".to_string(), "/tmp".to_string()]);
        for root in policy_paths_with_resolved(
            super::runtime_read_roots().map_err(|error| miette!("{error}"))?,
        )
        .into_iter()
        .chain(policy_paths_with_resolved(&policy.readable_roots))
        {
            push_existing_mount(args, "--ro-bind", &root);
        }
    }
    Ok(())
}

fn push_writable_root_mounts(args: &mut Vec<String>, policy: &FileSystemSandboxPolicy) {
    let flag = if policy.deny_all_writes {
        "--ro-bind"
    } else {
        "--bind"
    };
    for root in policy_paths_with_resolved(super::system_temporary_paths()) {
        push_existing_mount(args, flag, &root);
    }
    for writable_root in &policy.writable_roots {
        for root in policy_paths_with_resolved(std::slice::from_ref(&writable_root.root)) {
            push_existing_mount(args, flag, &root);
        }
    }
}

fn push_read_only_subpath_mounts(args: &mut Vec<String>, policy: &FileSystemSandboxPolicy) {
    for writable_root in &policy.writable_roots {
        for subpath in policy_paths_with_resolved(&writable_root.read_only_subpaths) {
            push_existing_mount(args, "--ro-bind", &protection_boundary(&subpath));
        }
    }
}

fn push_deny_write_mounts(args: &mut Vec<String>, policy: &FileSystemSandboxPolicy) {
    for path in policy_paths_with_resolved(&policy.deny_write_paths) {
        push_existing_mount(args, "--ro-bind", &protection_boundary(&path));
    }
}

fn push_deny_read_mounts(args: &mut Vec<String>, policy: &FileSystemSandboxPolicy) {
    let mut masked = Vec::<PathBuf>::new();
    for path in policy_paths_with_resolved(&policy.deny_read_paths) {
        let path = protection_boundary(&path);
        // An ancestor mask already hides every descendant; reopening a child
        // would only create needless mounts and risk changing precedence.
        if masked.iter().any(|parent| path.starts_with(parent)) {
            continue;
        }
        if path.is_dir() {
            args.extend([
                "--perms".into(),
                "000".into(),
                "--tmpfs".into(),
                path_to_arg(&path),
            ]);
            masked.push(path);
        } else if path.is_file() {
            push_mount(args, "--ro-bind", Path::new("/dev/null"), &path);
        }
    }
    for path in masked.into_iter().rev() {
        args.extend(["--remount-ro".into(), path_to_arg(&path)]);
    }
}

fn protection_boundary(path: &Path) -> PathBuf {
    let mut prefix = PathBuf::new();
    for component in path.components() {
        prefix.push(component);
        if prefix.is_file() {
            return prefix;
        }
    }
    path.to_owned()
}

fn validate_paths(policy: &FileSystemSandboxPolicy) -> Result<()> {
    super::validate_native_policy_paths(policy)
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
    use crate::WritableRoot;

    #[test]
    fn bwrap_args_protect_full_disk_policy_with_denies() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let protected_home = tempdir.path().join("private").join("daat");
        let source_root = tempdir.path().join("workspace").join("source");
        std::fs::create_dir_all(&protected_home).expect("create protected home");
        std::fs::create_dir_all(&source_root).expect("create source root");
        let policy = FileSystemSandboxPolicy {
            full_disk_read: true,
            deny_all_writes: false,
            full_disk_write: true,
            readable_roots: Vec::new(),
            writable_roots: Vec::new(),
            deny_read_paths: vec![protected_home.clone()],
            bound_roots: Vec::new(),
            deny_write_paths: vec![source_root.clone()],
        };

        let args = create_bwrap_args(&policy, vec!["sh".to_string(), "-lc".to_string()]).unwrap();

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
    fn ancestor_read_mask_covers_nested_denies_without_reopening_them() {
        let tempdir = tempfile::tempdir().unwrap();
        let private = tempdir.path().join("private");
        let nested = private.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("secret.txt"), "secret").unwrap();
        let mut policy = FileSystemSandboxPolicy::unrestricted();
        policy.deny_read_paths = vec![nested.clone(), private.clone(), nested.join("secret.txt")];
        let args = create_bwrap_args(&policy, vec!["/bin/sh".into()]).unwrap();
        assert!(contains_pair(&args, "--tmpfs", private.to_str().unwrap()));
        assert!(contains_pair(
            &args,
            "--remount-ro",
            private.to_str().unwrap()
        ));
        assert!(!contains_pair(&args, "--tmpfs", nested.to_str().unwrap()));
    }

    #[test]
    fn bwrap_args_reopen_writable_roots_under_read_only_root() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let workspace = tempdir.path().join("workspace");
        let git_dir = workspace.join(".git");
        std::fs::create_dir_all(&git_dir).expect("create git dir");
        let policy = FileSystemSandboxPolicy {
            full_disk_read: true,
            deny_all_writes: false,
            full_disk_write: false,
            readable_roots: Vec::new(),
            writable_roots: vec![WritableRoot {
                root: workspace.clone(),
                read_only_subpaths: vec![git_dir.clone()],
            }],
            deny_read_paths: Vec::new(),
            bound_roots: Vec::new(),
            deny_write_paths: Vec::new(),
        };

        let args = create_bwrap_args(&policy, vec!["sh".to_string()]).unwrap();

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
        let args = create_bwrap_args(&policy, vec!["sh".to_string()]).unwrap();
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
    async fn writable_root_preserves_private_devices_and_pid_namespace() {
        if !crate::test_support::native_sandbox_available() {
            return;
        }
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
        let policy = SandboxPolicy {
            filesystem,
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
            deny_all_writes: false,
            full_disk_write: true,
            readable_roots: Vec::new(),
            writable_roots: Vec::new(),
            deny_read_paths: Vec::new(),
            bound_roots: Vec::new(),
            deny_write_paths: Vec::new(),
        };

        assert!(!requires_backend(&policy));
    }

    #[test]
    fn readonly_override_requires_isolation_and_preserves_read_access_to_write_roots() {
        let temp = tempfile::tempdir().unwrap();
        let mut policy = FileSystemSandboxPolicy {
            full_disk_read: true,
            full_disk_write: true,
            deny_all_writes: true,
            readable_roots: vec![],
            writable_roots: vec![],
            deny_read_paths: vec![],
            deny_write_paths: vec![],
            bound_roots: vec![],
        };
        assert!(requires_backend(&policy));
        let args = create_bwrap_args(&policy, vec!["/bin/true".into()]).unwrap();
        assert!(contains_triplet(&args, "--ro-bind", "/", "/"));
        assert!(!contains_triplet(&args, "--bind", "/", "/"));
        policy.full_disk_read = false;
        policy.full_disk_write = false;
        policy.writable_roots.push(super::super::WritableRoot {
            root: temp.path().to_owned(),
            read_only_subpaths: vec![],
        });
        let args = create_bwrap_args(&policy, vec!["/bin/true".into()]).unwrap();
        let path = temp.path().to_str().unwrap();
        assert!(contains_triplet(&args, "--ro-bind", path, path));
        assert!(!contains_triplet(&args, "--bind", path, path));
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
