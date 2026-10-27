use super::*;

async fn execute(
    policy: &RuntimeSandboxPolicy,
    workspace: &Path,
    script: &str,
) -> TerminalToolResult {
    let mut app = TerminalApp::new();
    let mut result = app
        .exec_command_with_progress(
            TerminalExecCommandRequest {
                command: script.into(),
                session_id: None,
                workdir: Some(workspace.to_string_lossy().into_owned()),
                sandbox_policy: policy,
                yield_time_ms: Some(5000),
                max_chars: Some(16000),
            },
            |_, _| {},
        )
        .await
        .unwrap();
    let mut output = result.output.clone();
    for _ in 0..10 {
        if result.session.exit_code.is_some() {
            break;
        }
        result = app
            .write_stdin_with_progress(
                &result.session.session_id,
                String::new(),
                TerminalWaitMode::Timeout,
                Some(1000),
                Some(16000),
                |_, _| {},
            )
            .await
            .unwrap();
        output.push_str(&result.output);
    }
    assert_eq!(result.session.exit_code, Some(0), "{output}");
    result.output = output;
    result
}

#[tokio::test]
async fn portable_commands_are_available_to_terminal_children_and_reclaimed() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("input.txt"), "hay\nneedle\n").unwrap();
    let result = execute(
        &RuntimeSandboxPolicy::disabled(),
        directory.path(),
        r#"
        set -e
        env printenv PATH
        printf 'b\na\nb\n' | env sort | env uniq
        env cp input.txt copied.txt
        env grep -n needle copied.txt
    "#,
    )
    .await;
    let lines = result.output.lines().collect::<Vec<_>>();
    assert_eq!(&lines[1..], ["a", "b", "2:needle"]);
    assert_eq!(
        std::fs::read(directory.path().join("copied.txt")).unwrap(),
        b"hay\nneedle\n"
    );
    let commands = std::env::split_paths(lines[0]).next().unwrap();
    assert!(
        !commands.exists(),
        "finished session retained its command directory: {}",
        commands.display()
    );
}

#[tokio::test]
async fn portable_commands_obey_native_filesystem_authority() {
    if !crate::sandbox::test_support::native_sandbox_available() {
        return;
    }
    let workspace = super::non_temporary_scratch();
    let outside = super::non_temporary_scratch();
    std::fs::write(workspace.path().join("input.txt"), "needle\n").unwrap();
    std::fs::write(outside.path().join("private.txt"), "secret\n").unwrap();
    let mut policy = RuntimeSandboxPolicy::disabled();
    policy.strong_filesystem = crate::sandbox::StrongFilesystemSandboxMode::Required;
    policy.filesystem.full_disk_read = false;
    policy.filesystem.full_disk_write = false;
    policy.filesystem.readable_roots = vec![workspace.path().to_owned()];
    policy.filesystem.writable_roots = vec![crate::sandbox::WritableRoot {
        root: workspace.path().to_owned(),
        read_only_subpaths: vec![],
    }];
    policy.filesystem.deny_read_paths = vec![outside.path().to_owned()];
    let outside_path = outside
        .path()
        .to_string_lossy()
        .replace('\\', "/")
        .replace('\'', "'\\''");
    let script = format!(
        r#"
        set -e
        env cp input.txt copied.txt
        env rg needle copied.txt
        if env cp input.txt '{outside_path}/forbidden.txt'; then exit 40; fi
        set +e
        env grep secret '{outside_path}/private.txt'
        status=$?
        test "$status" -eq 2 || exit 41
        printf 'authority-preserved\n'
    "#
    );
    let result = execute(&policy, workspace.path(), &script).await;
    assert!(result.output.contains("authority-preserved"));
    assert!(result.output.lines().any(|line| line == "needle"));
    assert!(!outside.path().join("forbidden.txt").exists());
    assert_eq!(
        std::fs::read(outside.path().join("private.txt")).unwrap(),
        b"secret\n"
    );
}
