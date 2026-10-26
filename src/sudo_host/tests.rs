//! Exercise the native broker protocol without prompting for OS elevation.
use super::*;
use crate::{
    app::{ActorCallContext, AppToolExecutionContext},
    sandbox::{RuntimeSandboxPolicy, SandboxAsyncChild},
};

pub(crate) fn local_client() -> Arc<Client> {
    let (parent, helper) = tokio::io::duplex(256 * 1024);
    tokio::spawn(async move {
        let _ = serve(helper).await;
    });
    Client::from_stream(parent)
}
fn context(root: &Path, client: Arc<Client>) -> AppToolExecutionContext {
    let mut policy = RuntimeSandboxPolicy::disabled();
    policy.administrator = Some(client);
    AppToolExecutionContext {
        restart_native: false,
        access_broker: None,
        execution_cwd: root.into(),
        sandbox_policy: policy,
        tool_output_max_tokens: 2048,
        turn_epoch: 0,
        actor_call: Some(ActorCallContext {
            remote_read_only: false,
            actor_id: "native-probe".into(),
            message_id: "native-input".into(),
            device_policy: None,
        }),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn broker_filesystem_preserves_hash_freshness_and_hard_readonly_authority() {
    let root = tempfile::tempdir().unwrap();
    let client = local_client();
    let mut context = context(root.path(), client);
    let path = root.path().join("report.txt");
    let write = crate::file_io::WriteFileArgs {
        path: path.to_string_lossy().into(),
        content: "original".into(),
        expected_sha256: None,
    };
    let result = context.files().write_file(&write).await.unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "original");
    let stale = crate::file_io::WriteFileArgs {
        content: "replacement".into(),
        expected_sha256: Some("0".repeat(64)),
        ..write.clone()
    };
    assert!(context.files().write_file(&stale).await.is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "original");
    let read = context
        .files()
        .read_file(&crate::file_io::ReadFileArgs {
            path: write.path.clone(),
            start_line: None,
            line_count: None,
        })
        .await
        .unwrap();
    assert_eq!(read.sha256, result.sha256);
    assert_eq!(read.content, "original");
    context.sandbox_policy.filesystem.deny_all_writes = true;
    assert!(context.files().write_file(&write).await.is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "original");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn broker_process_preserves_stdio_exit_status_and_execution_identity() {
    let root = tempfile::tempdir().unwrap();
    let client = local_client();
    let context = context(root.path(), client);
    let policy = context.sandbox_policy.clone();
    #[cfg(windows)]
    let (program,args)=(std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32/WindowsPowerShell/v1.0/powershell.exe").to_string_lossy().into_owned(),
        vec!["-NoProfile".into(),"-Command".into(),"[Console]::Out.Write('out:'); [Console]::Error.Write('err:'); $line=[Console]::ReadLine(); [Console]::Out.Write($line); exit 7".into()]);
    #[cfg(unix)]
    let (program, args): (String, Vec<String>) = (
        "/bin/sh".into(),
        vec![
            "-c".into(),
            "printf 'out:'; printf 'err:' >&2; IFS= read -r line; printf '%s' \"$line\"; exit 7"
                .into(),
        ],
    );
    let mut child = tokio::task::spawn_blocking(move || {
        SandboxAsyncChild::spawn_shell(
            &policy,
            &program,
            args,
            SandboxProcessOptions {
                stdin: SandboxStdio::Piped,
                stdout: SandboxStdio::Piped,
                stderr: SandboxStdio::Piped,
                ..Default::default()
            },
        )
    })
    .await
    .unwrap()
    .unwrap();
    assert!(child.id().is_some());
    assert!(
        child
            .ensure_policy(&RuntimeSandboxPolicy::disabled())
            .is_err()
    );
    child.ensure_policy(&context.sandbox_policy).unwrap();
    let mut input = child.take_stdin().unwrap();
    input.write_all(b"line\n").await.unwrap();
    input.flush().await.unwrap();
    drop(input);
    let mut stdout = child.take_stdout().unwrap();
    let mut stderr = child.take_stderr().unwrap();
    let mut out = vec![];
    let mut err = vec![];
    tokio::time::timeout(Duration::from_secs(15), async {
        tokio::try_join!(stdout.read_to_end(&mut out), stderr.read_to_end(&mut err)).unwrap();
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert_eq!(status.code(), Some(7));
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(out, b"out:line");
    assert_eq!(err, b"err:");
}

#[tokio::test]
async fn broker_disconnect_is_terminal_and_never_replays_requests() {
    let (parent, mut helper) = tokio::io::duplex(65536);
    let client = Client::from_stream(parent);
    let seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let captured = seen.clone();
    tokio::spawn(async move {
        let _: RequestEnvelope = read_frame(&mut helper).await.unwrap();
        captured.fetch_add(1, Ordering::SeqCst);
        // Lose the response after an effect may have happened.
    });
    let first = client.clone();
    assert!(
        tokio::task::spawn_blocking(move || first.request(Request::Kill { process: 1 }))
            .await
            .unwrap()
            .is_err()
    );
    tokio::time::timeout(Duration::from_secs(2), async {
        while !client.is_closed() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let second = client.clone();
    assert!(
        tokio::task::spawn_blocking(move || second.request(Request::Kill { process: 1 }))
            .await
            .unwrap()
            .is_err()
    );
    assert_eq!(seen.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn broker_kill_bypasses_a_blocked_stdin_write_and_waits_for_cleanup() {
    let client = local_client();
    #[cfg(windows)]
    let (program, arguments) = (
        "powershell.exe",
        vec!["-NoProfile", "-Command", "Start-Sleep -Seconds 120"],
    );
    #[cfg(unix)]
    let (program, arguments) = ("/bin/sh", vec!["-c", "sleep 120"]);
    let spawning = client.clone();
    let value = tokio::task::spawn_blocking(move || {
        spawning.request(Request::Spawn {
            program: program.into(),
            arguments: arguments.into_iter().map(String::from).collect(),
            environment: vec![],
            cwd: None,
            stdin: true,
        })
    })
    .await
    .unwrap()
    .unwrap();
    let process = value["process"].as_u64().unwrap();
    let writing = client.clone();
    let write = tokio::task::spawn_blocking(move || {
        writing.request(Request::Write {
            process,
            bytes: vec![b'x'; 1024 * 1024],
            close: false,
        })
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !write.is_finished(),
        "fixture must fill the unread stdin pipe"
    );
    let killing = client.clone();
    tokio::time::timeout(
        Duration::from_secs(5),
        tokio::task::spawn_blocking(move || killing.request(Request::Kill { process })),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    assert!(write.await.unwrap().is_err());
    let polling = client.clone();
    assert!(
        tokio::task::spawn_blocking(move || polling.request(Request::Poll { process }))
            .await
            .unwrap()
            .is_err()
    );
    client.close();
}

#[tokio::test]
async fn broker_rejects_a_response_for_an_unrelated_request() {
    let (parent, mut helper) = tokio::io::duplex(65536);
    let client = Client::from_stream(parent);
    tokio::spawn(async move {
        let request: RequestEnvelope = read_frame(&mut helper).await.unwrap();
        write_frame(
            &mut helper,
            &ResponseEnvelope {
                id: request.id + 1,
                response: Response {
                    value: Some(json!("forged")),
                    error: None,
                },
            },
        )
        .await
        .unwrap();
    });
    let requesting = client.clone();
    assert!(
        tokio::task::spawn_blocking(move || requesting.request(Request::Kill { process: 1 }))
            .await
            .unwrap()
            .is_err()
    );
    assert!(client.is_closed());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn healthy_broker_keeps_long_operations_alive_without_replaying() {
    let (parent, helper) = tokio::io::duplex(65536);
    let client = Client::from_stream(parent);
    let seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let effects = seen.clone();
    let task = tokio::spawn(async move {
        let (mut reader, writer) = tokio::io::split(helper);
        let writer = Arc::new(tokio::sync::Mutex::new(writer));
        loop {
            let Ok(request) = read_frame::<_, RequestEnvelope>(&mut reader).await else {
                break;
            };
            let delay = !matches!(request.request, Request::Ping);
            if delay {
                effects.fetch_add(1, Ordering::SeqCst);
            }
            let writer = writer.clone();
            tokio::spawn(async move {
                if delay {
                    tokio::time::sleep(Duration::from_secs(31)).await;
                }
                let _ = write_frame(
                    &mut *writer.lock().await,
                    &ResponseEnvelope {
                        id: request.id,
                        response: Response {
                            value: Some(json!("completed")),
                            error: None,
                        },
                    },
                )
                .await;
            });
        }
    });
    let requesting = client.clone();
    let result = tokio::time::timeout(
        Duration::from_secs(40),
        tokio::task::spawn_blocking(move || {
            requesting.request(Request::File {
                name: "fixture".into(),
                cwd: PathBuf::new(),
                arguments: json!({}),
            })
        }),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    assert_eq!(result, json!("completed"));
    assert_eq!(seen.load(Ordering::SeqCst), 1);
    assert!(!client.is_closed());
    client.close();
    task.abort();
}

#[cfg(not(target_os = "android"))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires DAAT_LOCUS_TEST_COMPANION and interactive OS administrator authorization"]
async fn native_elevation_runs_only_the_helper_with_os_administrator_authority() {
    let executable = PathBuf::from(
        std::env::var_os("DAAT_LOCUS_TEST_COMPANION")
            .expect("build the production companion first"),
    );
    let ordinary = transport::is_administrator().unwrap();
    let stream = transport::launch_executable(executable).await.unwrap();
    let client = Client::from_stream(stream);
    #[cfg(windows)]
    let (program, args) = (
        "powershell.exe",
        vec![
            "-NoProfile",
            "-Command",
            "[Console]::Out.Write(([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator))",
        ],
    );
    #[cfg(unix)]
    let (program, args) = ("/usr/bin/id", vec!["-u"]);
    let spawning = client.clone();
    let mut child = tokio::task::spawn_blocking(move || {
        BrokerChild::spawn(
            spawning,
            program,
            args.into_iter().map(String::from).collect(),
            SandboxProcessOptions {
                stdout: SandboxStdio::Piped,
                stderr: SandboxStdio::Piped,
                ..Default::default()
            },
        )
    })
    .await
    .unwrap()
    .unwrap();
    let mut output = vec![];
    tokio::time::timeout(
        Duration::from_secs(15),
        child.take_stdout().unwrap().read_to_end(&mut output),
    )
    .await
    .unwrap()
    .unwrap();
    #[cfg(windows)]
    assert_eq!(String::from_utf8(output).unwrap().trim(), "True");
    #[cfg(unix)]
    assert_eq!(String::from_utf8(output).unwrap().trim(), "0");
    assert_eq!(
        transport::is_administrator().unwrap(),
        ordinary,
        "the service must retain its original OS identity"
    );
    drop(child);
    client.close();
}

#[cfg(all(unix, not(target_os = "android")))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires DAAT_LOCUS_TEST_COMPANION and passwordless sudo on a disposable Unix runner"]
async fn root_helper_reads_protected_files_without_elevating_the_service() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let ordinary = unsafe { libc::geteuid() };
    assert_ne!(ordinary, 0, "run the service-side test as an ordinary user");
    let executable = PathBuf::from(
        std::env::var_os("DAAT_LOCUS_TEST_COMPANION")
            .expect("build the production companion first"),
    );
    let directory = tempfile::Builder::new()
        .prefix("dl-sudo-test-")
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let endpoint = directory.path().join("native.sock");
    let listener = tokio::net::UnixListener::bind(&endpoint).unwrap();
    std::fs::set_permissions(&endpoint, std::fs::Permissions::from_mode(0o600)).unwrap();
    // The disposable runner grants native root through sudo. Interactive
    // pkexec/AppleScript authorization remains a separate opt-in check.
    let mut helper = tokio::process::Command::new("/usr/bin/sudo")
        .arg("-n")
        .arg(&executable)
        .arg("sudo-host")
        .arg("--endpoint")
        .arg(&endpoint)
        .arg("--parent-pid")
        .arg(std::process::id().to_string())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let (stream, _) = tokio::select! {
        connected = listener.accept() => connected.unwrap(),
        status = helper.wait() => panic!("root helper exited before connecting: {status:?}"),
        () = tokio::time::sleep(Duration::from_secs(15)) => panic!("root helper did not connect"),
    };
    assert_eq!(stream.peer_cred().unwrap().uid(), 0);
    let client = Client::from_stream(stream);
    let spawning = client.clone();
    let cwd = directory.path().to_path_buf();
    let mut child = tokio::task::spawn_blocking(move || {
        BrokerChild::spawn(
            spawning,
            "/bin/sh",
            vec![
                "-c".into(),
                "umask 077; printf root-only > protected.txt; /usr/bin/id -u".into(),
            ],
            SandboxProcessOptions {
                current_dir: Some(cwd),
                stdout: SandboxStdio::Piped,
                ..Default::default()
            },
        )
    })
    .await
    .unwrap()
    .unwrap();
    let mut output = vec![];
    tokio::time::timeout(
        Duration::from_secs(15),
        child.take_stdout().unwrap().read_to_end(&mut output),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(String::from_utf8(output).unwrap().trim(), "0");
    let protected = directory.path().join("protected.txt");
    assert_eq!(std::fs::metadata(&protected).unwrap().uid(), 0);
    assert!(
        std::fs::read(&protected).is_err(),
        "the ordinary service must not inherit helper filesystem access"
    );
    let context = context(directory.path(), client.clone());
    let read = context
        .files()
        .read_file(&crate::file_io::ReadFileArgs {
            path: protected.to_string_lossy().into_owned(),
            start_line: None,
            line_count: None,
        })
        .await
        .unwrap();
    assert_eq!(read.content, "root-only");
    assert_eq!(unsafe { libc::geteuid() }, ordinary);
    drop(child);
    client.close();
    tokio::time::timeout(Duration::from_secs(5), helper.wait())
        .await
        .unwrap()
        .unwrap();
}
