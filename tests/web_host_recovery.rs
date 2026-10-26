//! The native client keeps stdin open while waiting for its helper to reconnect.
use daat_locus::{
    actors::ActorSystem,
    config::Config,
    daemon::control::{LaunchOptions, Server},
    service::ServiceState,
    web_host::Host,
};
use daat_locus_client_protocol::web::{Operation, Reply, Request};
use std::{path::Path, process::Stdio, sync::Arc, time::Duration};
use tokio::{io::AsyncReadExt, process::Child};

async fn attach(root: &Path, host: &Host) -> Child {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_daat-locus"));
    command
        .arg("web-host")
        .env("DAAT_LOCUS_HOME", root)
        .env_remove("DAAT_LOCUS_PROFILE_NAME")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    let child = command.spawn().unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while !host.available() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("native helper must attach");
    child
}

async fn receive(child: &mut Child) -> Request {
    let stdout = child.stdout.as_mut().unwrap();
    let size = stdout.read_u32().await.unwrap();
    assert!(size <= 65536);
    let mut bytes = vec![0; size as usize];
    stdout.read_exact(&mut bytes).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

async fn close(
    host: Arc<Host>,
) -> miette::Result<Option<daat_locus_client_protocol::web::Capture>> {
    host.request("a".repeat(64), Operation::Close).await
}

#[tokio::test]
async fn helper_exits_with_stdin_open_and_fresh_connections_do_not_replay_calls() {
    use tokio::io::AsyncWriteExt;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_owned();
    let workspace = root.join("workspace");
    let system =
        ActorSystem::open_account_gated(root.join("actors"), workspace.clone(), Config::default())
            .await
            .unwrap();
    let host = system.web_host();
    let origin = "http://127.0.0.1:41000".to_owned();
    let state = ServiceState::new(system.clone(), origin.clone()).unwrap();
    for (pending_call, slow_call) in [(false, false), (true, true), (true, false), (false, false)] {
        let server = Server::bind(
            root.clone(),
            root.clone(),
            LaunchOptions {
                profile: None,
                listen: "127.0.0.1:41000".parse().unwrap(),
                workspace: workspace.clone(),
            },
            origin.clone(),
            Some(state.clone()),
        )
        .await
        .unwrap();
        let mut child = attach(&root, &host).await;

        // A fresh helper must receive this new call rather than replay the call
        // whose reply was lost with the previous connection.
        let call = tokio::spawn(close(host.clone()));
        let request = tokio::time::timeout(Duration::from_secs(5), receive(&mut child))
            .await
            .unwrap();
        let reply = serde_json::to_vec(&Reply {
            id: request.id,
            capture: None,
            browser: None,
            error: None,
        })
        .unwrap();
        let stdin = child.stdin.as_mut().unwrap();
        stdin.write_u32(reply.len() as u32).await.unwrap();
        stdin.write_all(&reply).await.unwrap();
        stdin.flush().await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(5), call)
                .await
                .unwrap()
                .unwrap()
                .unwrap()
                .is_none()
        );

        let interrupted = if pending_call {
            let call = tokio::spawn(close(host.clone()));
            tokio::time::timeout(Duration::from_secs(5), receive(&mut child))
                .await
                .unwrap();
            Some(call)
        } else {
            None
        };
        // Child::wait closes its own stdin automatically. Keep the pipe outside
        // Child so the test reproduces the native client's open connection.
        let stdin = child.stdin.take().unwrap();
        if slow_call {
            // A slow native operation must survive the former outer deadline.
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(120)).await;
            assert!(!interrupted.as_ref().unwrap().is_finished());
            tokio::time::resume();
            assert!(host.available());
        }
        drop(server);
        // Do not close child.stdin: the real client is waiting on stdout EOF.
        let exit = tokio::time::timeout(Duration::from_secs(5), child.wait())
            .await
            .expect("disconnected helper must exit even while its stdin remains open")
            .unwrap();
        assert!(exit.success());
        drop(stdin);
        if let Some(call) = interrupted {
            assert!(
                tokio::time::timeout(Duration::from_secs(5), call)
                    .await
                    .unwrap()
                    .unwrap()
                    .is_err()
            );
        }
        assert!(!host.available());
    }
    system.shutdown().await;
}
