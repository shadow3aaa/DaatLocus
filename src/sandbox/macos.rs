use std::path::PathBuf;

use miette::Result;

use super::{RuntimeSandboxPolicy, SandboxSpawnSpec};

const SANDBOX_EXECUTABLE: &str = "/usr/bin/sandbox-exec";

const SEATBELT_BASE_POLICY: &str = r#"(version 1)
(deny default)
(allow process-exec)
(allow process-fork)
(allow signal (target same-sandbox))
(allow process-info* (target same-sandbox))
(allow sysctl-read)
(allow ipc-posix-sem)
(allow mach-lookup)
(allow system-socket)
(allow pseudo-tty)
(allow file-read* file-write* file-ioctl (literal "/dev/ptmx"))
(allow file-read* file-write* (regex "^/dev/ttys[0-9]+$"))
(allow file-ioctl (regex "^/dev/ttys[0-9]+$"))
(allow file-read* file-test-existence file-read-metadata (subpath "/dev"))
(allow file-read* file-write* (literal "/dev/null"))
(allow file-read* file-write* (literal "/dev/zero"))
(allow file-read* file-write* (literal "/dev/tty"))
(allow file-read-data file-write-data (subpath "/dev/fd"))
(allow network-inbound (local ip))
(allow network-outbound)
"#;

pub fn wrap_shell_command(
    policy: &RuntimeSandboxPolicy,
    program: &str,
    args: Vec<String>,
) -> Result<SandboxSpawnSpec> {
    let mut sections = vec![SEATBELT_BASE_POLICY.to_string()];
    sections.push(render_read_policy(policy));
    sections.push(render_write_policy(policy));
    sections.push(render_protected_path_denies(policy));

    let profile = sections
        .into_iter()
        .filter(|section| !section.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");

    let mut sandbox_args = vec!["-p".to_string(), profile, "--".to_string(), program.to_string()];
    sandbox_args.extend(args);
    Ok(SandboxSpawnSpec {
        program: PathBuf::from(SANDBOX_EXECUTABLE),
        args: sandbox_args,
    })
}

fn render_read_policy(policy: &RuntimeSandboxPolicy) -> String {
    let fs = &policy.filesystem;
    let mut lines = Vec::new();
    if fs.full_disk_read {
        lines.push(
            "(allow file-read* file-map-executable file-test-existence file-read-metadata (subpath \"/\"))"
                .to_string(),
        );
    } else {
        for root in &fs.readable_roots {
            lines.push(format!(
                "(allow file-read* file-map-executable file-test-existence file-read-metadata (subpath \"{}\"))",
                root.display()
            ));
        }
        for root in &fs.writable_roots {
            lines.push(format!(
                "(allow file-read* file-map-executable file-test-existence file-read-metadata (subpath \"{}\"))",
                root.root.display()
            ));
        }
    }
    lines.join("\n")
}

fn render_write_policy(policy: &RuntimeSandboxPolicy) -> String {
    let fs = &policy.filesystem;
    let mut lines = Vec::new();
    if fs.full_disk_write {
        lines.push("(allow file-write* (subpath \"/\"))".to_string());
    } else {
        for root in &fs.writable_roots {
            lines.push(format!(
                "(allow file-write* file-read-metadata file-test-existence (subpath \"{}\"))",
                root.root.display()
            ));
            for subpath in &root.read_only_subpaths {
                lines.push(format!(
                    "(deny file-write* (subpath \"{}\"))",
                    subpath.display()
                ));
            }
        }
    }
    lines.join("\n")
}

fn render_protected_path_denies(policy: &RuntimeSandboxPolicy) -> String {
    let mut lines = Vec::new();
    for path in &policy.filesystem.deny_read_paths {
        lines.push(format!(
            "(deny file-read* file-map-executable file-test-existence file-read-metadata (subpath \"{}\"))",
            path.display()
        ));
    }
    for path in &policy.filesystem.deny_write_paths {
        lines.push(format!("(deny file-write* (subpath \"{}\"))", path.display()));
    }
    lines.join("\n")
}
