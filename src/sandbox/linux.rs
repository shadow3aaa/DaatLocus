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

    args.push("--dev".to_string());
    args.push("/dev".to_string());
    args.push("--proc".to_string());
    args.push("/proc".to_string());
}

fn push_writable_root_mounts(args: &mut Vec<String>, policy: &FileSystemSandboxPolicy) {
    for writable_root in &policy.writable_roots {
        for root in policy_paths_with_resolved(std::slice::from_ref(&writable_root.root)) {
            push_existing_mount(args, "--bind", &root);
        }
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
    for path in policy_paths_with_resolved(&policy.deny_read_paths) {
        if path.is_dir() {
            args.push("--tmpfs".to_string());
            args.push(path_to_arg(&path));
            if path_is_denied(&path, &policy.deny_write_paths) {
                args.push("--remount-ro".to_string());
                args.push(path_to_arg(&path));
            }
        } else if path.is_file() {
            push_mount(args, "--ro-bind", Path::new("/dev/null"), &path);
        }
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
    fn bwrap_args_hide_read_and_write_denied_directories_as_read_only() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let protected_home = tempdir.path().join(".daat-locus");
        std::fs::create_dir_all(&protected_home).expect("create protected home");
        let policy = FileSystemSandboxPolicy {
            full_disk_read: true,
            full_disk_write: true,
            readable_roots: Vec::new(),
            writable_roots: Vec::new(),
            deny_read_paths: vec![protected_home.clone()],
            deny_write_paths: vec![protected_home.clone()],
        };

        let args = create_bwrap_args(&policy, vec!["sh".to_string()]);

        let protected_home = protected_home.to_string_lossy();
        assert!(contains_pair(&args, "--tmpfs", protected_home.as_ref()));
        assert!(contains_pair(
            &args,
            "--remount-ro",
            protected_home.as_ref()
        ));
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
    fn no_backend_needed_for_unrestricted_policy() {
        let policy = FileSystemSandboxPolicy {
            full_disk_read: true,
            full_disk_write: true,
            readable_roots: Vec::new(),
            writable_roots: Vec::new(),
            deny_read_paths: Vec::new(),
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
