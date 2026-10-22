//! Application authority checks stay here, not in the independent native library.

use super::{
    RuntimeSandboxPolicy, SandboxAsyncChild, SandboxProcessOptions, SandboxStdio,
    StrongFilesystemSandboxMode, WritableRoot,
};
use std::{io, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn sandbox_tcp_socket_cannot_claim_local_owner_authority() {
    if !super::test_support::native_sandbox_available() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let mut policy = RuntimeSandboxPolicy::disabled();
    policy.strong_filesystem = StrongFilesystemSandboxMode::Required;
    policy.filesystem.full_disk_read = false;
    policy.filesystem.full_disk_write = false;
    policy.filesystem.readable_roots = vec![root.clone()];
    policy.filesystem.writable_roots = vec![WritableRoot {
        root: root.clone(),
        read_only_subpaths: vec![],
    }];
    policy.filesystem.bound_roots = vec![root.clone()];
    let (program, args) = socket_command(address.port());
    let mut child = SandboxAsyncChild::spawn_shell(
        &policy,
        &program,
        args,
        SandboxProcessOptions {
            current_dir: Some(root),
            stdin: SandboxStdio::Piped,
            stdout: SandboxStdio::Piped,
            stderr: SandboxStdio::Piped,
            ..Default::default()
        },
    )
    .unwrap();
    let (_socket, peer) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
        .await
        .unwrap()
        .unwrap();
    assert!(
        !crate::accounts::local_owner_connection(peer, Some(address)),
        "sandboxed process claimed OS-owner bootstrap authority"
    );
    child
        .take_stdin()
        .unwrap()
        .write_all(b"done\n")
        .await
        .unwrap();
    let mut out = child.take_stdout().unwrap();
    let mut err = child.take_stderr().unwrap();
    let (stdout, stderr, status) = tokio::time::timeout(Duration::from_secs(20), async {
        tokio::try_join!(
            async {
                let mut text = String::new();
                out.read_to_string(&mut text).await?;
                Ok::<_, io::Error>(text)
            },
            async {
                let mut text = String::new();
                err.read_to_string(&mut text).await?;
                Ok::<_, io::Error>(text)
            },
            async {
                loop {
                    if let Some(status) = child.try_wait()? {
                        return Ok::<_, io::Error>(status);
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
        )
    })
    .await
    .expect("sandbox authority probe timed out")
    .unwrap();
    assert!(status.success(), "{status};stdout={stdout};stderr={stderr}");
}

#[cfg(target_os = "linux")]
fn socket_command(port: u16) -> (String, Vec<String>) {
    (
        "/bin/bash".into(),
        vec![
            "-c".into(),
            format!("exec 3<>/dev/tcp/127.0.0.1/{port}; read command"),
        ],
    )
}

#[cfg(target_os = "windows")]
fn socket_command(port: u16) -> (String, Vec<String>) {
    let mut buffer = vec![0u16; 32768];
    let length = unsafe {
        windows_sys::Win32::System::SystemInformation::GetWindowsDirectoryW(
            buffer.as_mut_ptr(),
            buffer.len() as u32,
        )
    } as usize;
    assert!(length > 0 && length < buffer.len());
    use std::os::windows::ffi::OsStringExt;
    let shell = std::path::PathBuf::from(std::ffi::OsString::from_wide(&buffer[..length]))
        .join("System32/WindowsPowerShell/v1.0/powershell.exe");
    (
        shell.to_string_lossy().into_owned(),
        vec![
            "-NoProfile".into(),
            "-NonInteractive".into(),
            "-Command".into(),
            format!(
                "$ErrorActionPreference='Stop'; $client=[Net.Sockets.TcpClient]::new('127.0.0.1',{port}); $null=[Console]::ReadLine(); $client.Dispose()"
            ),
        ],
    )
}
