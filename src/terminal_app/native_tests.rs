#[tokio::test]
async fn terminal_shell_runs_inside_required_native_isolation() {
    if !crate::sandbox::test_support::native_sandbox_available() {
        return;
    }
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use windows_sys::Win32::Storage::FileSystem::GetShortPathNameW;

    let workspace = tempfile::Builder::new()
        .prefix("native sandbox workspace ")
        .tempdir()
        .unwrap();
    // Hosted Windows runners can expose Temp through an 8.3 user-profile
    // alias. Exercise the OS spelling when the volume supplies one.
    let path = workspace
        .path()
        .as_os_str()
        .encode_wide()
        .chain([0])
        .collect::<Vec<_>>();
    let mut short = vec![0u16; 32768];
    let length =
        unsafe { GetShortPathNameW(path.as_ptr(), short.as_mut_ptr(), short.len() as u32) };
    assert!(length > 0 && length < short.len() as u32);
    let workdir =
        std::path::PathBuf::from(std::ffi::OsString::from_wide(&short[..length as usize]));
    let mut policy = crate::sandbox::RuntimeSandboxPolicy::disabled();
    policy.strong_filesystem = crate::sandbox::StrongFilesystemSandboxMode::Required;
    policy.filesystem.full_disk_read = false;
    policy.filesystem.full_disk_write = false;
    policy.filesystem.readable_roots = vec![workspace.path().to_path_buf()];
    policy.filesystem.writable_roots = vec![crate::sandbox::WritableRoot {
        root: workspace.path().to_path_buf(),
        read_only_subpaths: vec![],
    }];
    let mut process = daat_locus_bash::process::TerminalProcess::spawn(
        "printf 'native-terminal-ok\\n'",
        workdir.to_str(),
        &policy,
    )
    .expect("embedded Rubash subprocess must start with required MXC/PSEC isolation");
    let status = tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            if let Some(status) = process.try_wait().unwrap() {
                break status;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("terminal exited");
    assert!(
        process
            .wait_for_output_drained(std::time::Duration::from_secs(2))
            .await
    );
    let output = process.output_since(0).text;
    assert!(status.success(), "status={status}, output={output:?}");
    assert!(output.contains("native-terminal-ok"));
}

#[cfg(windows)]
#[tokio::test]
async fn terminal_shell_renames_and_deletes_closed_logs_across_invocations() {
    if !crate::sandbox::test_support::native_sandbox_available() {
        return;
    }
    let workspace = tempfile::Builder::new()
        .prefix("terminal mutation workspace ")
        .tempdir()
        .unwrap();
    let mut policy = crate::sandbox::RuntimeSandboxPolicy::disabled();
    policy.strong_filesystem = crate::sandbox::StrongFilesystemSandboxMode::Required;
    policy.filesystem.full_disk_read = true;
    policy.filesystem.full_disk_write = false;
    policy.filesystem.readable_roots = vec![workspace.path().to_path_buf()];
    policy.filesystem.writable_roots = vec![crate::sandbox::WritableRoot {
        root: workspace.path().to_path_buf(),
        read_only_subpaths: vec![],
    }];
    let commands = [
        (
            "create redirected logs",
            "printf 'check\\n' > check.log; printf 'baseline\\n' > baseline.log; [[ $(<check.log) == check && $(<baseline.log) == baseline ]]",
        ),
        (
            "rename and delete logs from the previous invocation",
            "cmd.exe /c 'ren check.log renamed-check.log && del renamed-check.log baseline.log'",
        ),
    ];
    for (index, (stage, command)) in commands.into_iter().enumerate() {
        let mut process = daat_locus_bash::process::TerminalProcess::spawn(
            command,
            workspace.path().to_str(),
            &policy,
        )
        .unwrap_or_else(|error| panic!("{stage}: {error}"));
        let status = tokio::time::timeout(std::time::Duration::from_secs(30), async {
            loop {
                if let Some(status) = process.try_wait().unwrap() {
                    break status;
                }
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{stage}: terminal did not exit"));
        assert!(
            process
                .wait_for_output_drained(std::time::Duration::from_secs(2))
                .await,
            "{stage}: output did not drain"
        );
        let output = process.output_since(0).text;
        assert!(
            status.success(),
            "{stage}: status={status}, output={output:?}"
        );
        // End the entire previous native process before opening the next one.
        drop(process);
        if index == 0 {
            for name in ["check.log", "baseline.log"] {
                assert!(
                    workspace.path().join(name).is_file(),
                    "{name} was not created"
                );
            }
        }
    }
    for name in ["check.log", "renamed-check.log", "baseline.log"] {
        assert!(
            !workspace.path().join(name).exists(),
            "{name} was not removed"
        );
    }
}
