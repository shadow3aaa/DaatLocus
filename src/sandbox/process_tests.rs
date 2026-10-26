//! Host tools and native programs enforce the same explicit filesystem scopes.
use super::{RuntimeSandboxPolicy, StrongFilesystemSandboxMode, WritableRoot};
use std::{path::Path, time::Duration};

#[tokio::test]
async fn terminal_requires_the_same_read_scope_as_files_and_keeps_external_writes_denied() {
    if !super::test_support::native_sandbox_available() {
        return;
    }
    let temp = crate::sandbox::test_support::scoped_directory();
    let root = temp.path().join("work");
    let tools = temp.path().join("tools");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(&tools).unwrap();
    let outside = tools.join("data.txt");
    std::fs::write(&outside, "outside fixture").unwrap();
    let mut policy = RuntimeSandboxPolicy::disabled();
    policy.strong_filesystem = StrongFilesystemSandboxMode::Required;
    policy.filesystem.full_disk_read = false;
    policy.filesystem.full_disk_write = false;
    policy.filesystem.readable_roots = vec![root.clone()];
    policy.filesystem.writable_roots = vec![WritableRoot {
        root: root.clone(),
        read_only_subpaths: vec![],
    }];
    assert!(
        crate::file_io::read_file(
            &root,
            &policy,
            &crate::file_io::ReadFileArgs {
                path: outside.to_string_lossy().into_owned(),
                start_line: None,
                line_count: None,
            }
        )
        .is_err()
    );
    let command = command(&tools, &outside);
    for granted in [false, true] {
        let mut process_policy = policy.clone();
        if granted {
            process_policy.filesystem.readable_roots.push(tools.clone());
            let file = crate::file_io::read_file(
                &root,
                &process_policy,
                &crate::file_io::ReadFileArgs {
                    path: outside.to_string_lossy().into_owned(),
                    start_line: None,
                    line_count: None,
                },
            )
            .unwrap();
            assert_eq!(file.content, "outside fixture");
        }
        let mut process = daat_locus_bash::process::TerminalProcess::spawn(
            &command,
            root.to_str(),
            &process_policy,
        )
        .unwrap();
        let status = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if let Some(status) = process.try_wait().unwrap() {
                    break status;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("native process timed out");
        assert!(
            process
                .wait_for_output_drained(Duration::from_secs(2))
                .await
        );
        let output = process.output_since(0).text;
        if !granted {
            assert!(
                !status.success(),
                "ungranted external program executed: {output}"
            );
            continue;
        }
        assert!(status.success(), "{status}: {output}");
        assert!(output.contains("external-tool-ran"), "{output}");
        assert!(output.contains("outside fixture"), "{output}");
        assert!(output.contains("outside-write-denied"), "{output}");
        assert_eq!(
            std::fs::read_to_string(&outside).unwrap(),
            "outside fixture"
        );
        assert!(!tools.join("created.txt").exists());
        assert_eq!(
            std::fs::read_to_string(root.join("output.txt")).unwrap(),
            "workspace output"
        );
    }
    assert!(!policy.is_path_readable(&outside));
}

fn command(tools: &Path, outside: &Path) -> String {
    let quote = |path: &Path| format!("'{}'", path.display().to_string().replace('\'', "'\"'\"'"));
    let executable = tools.join(if cfg!(windows) {
        "dl-external-tool.cmd"
    } else {
        "dl-external-tool"
    });
    std::fs::write(
        &executable,
        if cfg!(windows) {
            "@echo off\r\necho external-tool-ran\r\n"
        } else {
            "#!/bin/sh\nprintf 'external-tool-ran\\n'\n"
        },
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    format!(
        r#"set -eu; export PATH={}:"$PATH"; '{}'; while IFS= read -r line || [[ -n $line ]]; do printf '%s\n' "$line"; done < {}; for path in {} {}; do if (printf bad > "$path") 2>/dev/null; then exit 20; fi; done; printf '\noutside-write-denied\n'; printf 'workspace output' > output.txt"#,
        quote(tools),
        executable.display(),
        quote(outside),
        quote(outside),
        quote(&tools.join("created.txt"))
    )
}
