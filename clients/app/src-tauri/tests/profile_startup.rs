//! Opt-in real macOS client startup with a long, aliased profile path.
#![cfg(target_os = "macos")]

use std::{
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

struct Client(Child);
impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
struct Service {
    executable: PathBuf,
    namespace: PathBuf,
    socket: PathBuf,
}
impl Drop for Service {
    fn drop(&mut self) {
        let _ = Command::new(&self.executable)
            .args(["--profile", "long-runtime", "stop"])
            .env("DAAT_LOCUS_HOME", &self.namespace)
            .output();
        let _ = std::fs::remove_file(&self.socket);
    }
}

#[test]
#[ignore = "requires a macOS desktop and the adjacent companion service binary"]
fn long_aliased_profile_starts_and_recovers_after_client_interruption() {
    use std::os::unix::{ffi::OsStrExt, net::UnixStream};
    let temp = tempfile::tempdir().unwrap();
    let namespace = temp.path().join("long-profile-".repeat(12));
    let root = namespace.join("profiles/long-runtime");
    assert!(
        root.join("runtime/computer-use.sock")
            .as_os_str()
            .as_bytes()
            .len()
            >= 104
    );
    let app = Path::new(env!("CARGO_BIN_EXE_daat-locus-app"));
    let executable = app.with_file_name("daat-locus");
    assert!(
        executable.is_file(),
        "Build the adjacent companion service first"
    );
    let started = Command::new(&executable)
        .args([
            "--profile",
            "long-runtime",
            "start",
            "--listen",
            "127.0.0.1:0",
            "--workspace",
        ])
        .arg(temp.path().join("workspace"))
        .env("DAAT_LOCUS_HOME", &namespace)
        .output()
        .unwrap();
    assert!(
        started.status.success(),
        "{}",
        String::from_utf8_lossy(&started.stderr)
    );
    let socket = daat_locus_client_protocol::computer_use::socket_path(&root);
    let service = Service {
        executable,
        namespace,
        socket,
    };
    let metadata = root.join("runtime/daemon.json");
    let pid = || {
        serde_json::from_slice::<serde_json::Value>(&std::fs::read(&metadata).unwrap()).unwrap()["pid"].clone()
    };
    let original_pid = pid();
    for _ in 0..2 {
        let mut client = Client(
            Command::new(app)
                .args(["--profile", "long-runtime"])
                .env("DAAT_LOCUS_HOME", &service.namespace)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            assert!(
                client.0.try_wait().unwrap().is_none(),
                "Client aborted before its GUI host was ready"
            );
            if UnixStream::connect(&service.socket).is_ok() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "Long-profile GUI host did not start"
            );
            thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(
            pid(),
            original_pid,
            "Client startup replaced the independent service"
        );
        drop(client); // Deliberate interruption leaves a stale endpoint to recover.
    }
}
