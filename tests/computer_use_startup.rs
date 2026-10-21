#![cfg(windows)]
//! Opt-in desktop coverage of the real service readiness boundary.
use daat_locus::daemon::control;
use daat_locus_virtual_workspace::displays;
use std::{
    os::windows::process::CommandExt,
    process::{Child, Command, Stdio},
    time::Duration,
};

struct Service(Child);
impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test]
#[ignore = "requires installed signed IDD and an unlocked stationary desktop; run native desktop tests serially"]
async fn service_readiness_includes_a_virtual_display_without_any_computer_tool_call() {
    let temporary = tempfile::tempdir().unwrap();
    let profile = temporary.path().join("profile");
    let before = displays().unwrap();
    let mut service = Service(
        Command::new(env!("CARGO_BIN_EXE_daat-locus"))
            .args(["serve", "--listen", "127.0.0.1:0", "--workspace"])
            .arg(temporary.path().join("workspace"))
            .env("DAAT_LOCUS_HOME", &profile)
            .env_remove("DAAT_LOCUS_PROFILE_NAME")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(0x08000000)
            .spawn()
            .unwrap(),
    );
    let status = tokio::time::timeout(Duration::from_secs(45), control::wait_ready(&profile))
        .await
        .expect("service did not become ready")
        .unwrap();
    assert_eq!(status.pid, service.0.id());
    let ready = displays().unwrap();
    let added: Vec<_> = ready
        .iter()
        .filter(|display| !before.iter().any(|old| old.id == display.id))
        .collect();
    assert_eq!(
        added.len(),
        1,
        "service readiness must include one virtual display; verify the signed IDD prerequisite"
    );
    let screen = added[0];
    assert_eq!(screen.bounds.width().unwrap(), 1920);
    assert_eq!(screen.bounds.height().unwrap(), 1080);
    // No account or Actor tools were used, and an idle service retains the output.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(displays().unwrap(), ready);
    control::stop(&profile).await.unwrap();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Some(exit) = service.0.try_wait().unwrap() {
                assert!(exit.success());
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("service did not stop");
    let after = displays().unwrap();
    assert!(after.iter().all(|display| display.id != screen.id));
    assert_eq!(after.len(), before.len());
    assert!(before.iter().all(|display| after.contains(display)));
}
