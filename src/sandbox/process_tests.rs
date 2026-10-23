//! The App policy stays scoped while native programs receive host read access.
use super::{RuntimeSandboxPolicy, StrongFilesystemSandboxMode, WritableRoot};
use std::{path::Path, time::Duration};

#[tokio::test]
async fn terminal_reads_and_executes_outside_workspace_but_cannot_write_there() {
    if !super::test_support::native_sandbox_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
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
    let mut process =
        crate::terminal_app::process::TerminalProcess::spawn(&command, root.to_str(), &policy)
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
    assert!(!policy.is_path_readable(&outside));
}

#[cfg(windows)]
fn command(tools: &Path, outside: &Path) -> String {
    let quote = |path: &Path| format!("'{}'", path.display().to_string().replace('\'', "''"));
    std::fs::write(
        tools.join("dl-external-tool.cmd"),
        "@echo off\r\necho external-tool-ran\r\n",
    )
    .unwrap();
    format!(
        r#"$ErrorActionPreference='Stop'; $env:PATH={}+';'+$env:PATH; dl-external-tool.cmd; if ($LASTEXITCODE -ne 0) {{exit 10}}; [Console]::WriteLine([IO.File]::ReadAllText({})); foreach ($path in @({}, {})) {{ $denied=$false; try {{ [IO.File]::WriteAllText($path, 'bad') }} catch [UnauthorizedAccessException] {{ $denied=$true }}; if (-not $denied) {{exit 20}} }}; [Console]::WriteLine('outside-write-denied'); [IO.File]::WriteAllText((Join-Path (Get-Location) 'output.txt'), 'workspace output')"#,
        quote(tools),
        quote(outside),
        quote(outside),
        quote(&tools.join("created.txt"))
    )
}

#[cfg(unix)]
fn command(tools: &Path, outside: &Path) -> String {
    use std::os::unix::fs::PermissionsExt;
    let quote = |path: &Path| format!("'{}'", path.display().to_string().replace('\'', "'\"'\"'"));
    let executable = tools.join("dl-external-tool");
    std::fs::write(&executable, "#!/bin/sh\nprintf 'external-tool-ran\\n'\n").unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    format!(
        r#"set -eu; export PATH={}:"$PATH"; dl-external-tool; cat {}; for path in {} {}; do if (printf bad > "$path") 2>/dev/null; then exit 20; fi; done; printf '\noutside-write-denied\n'; printf 'workspace output' > output.txt"#,
        quote(tools),
        quote(outside),
        quote(outside),
        quote(&tools.join("created.txt"))
    )
}
