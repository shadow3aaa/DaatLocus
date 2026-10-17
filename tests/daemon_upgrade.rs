//! A running service must remain controllable when an installer replaces its executable.
use daat_locus::daemon::control::{self, Action};
use std::{
    path::PathBuf,
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
async fn replacing_a_running_executable_preserves_control_and_rejects_a_stale_process_generation() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("profile");
    let source = PathBuf::from(env!("CARGO_BIN_EXE_daat-locus"));
    let installed = temp.path().join(if cfg!(windows) {
        "service.exe"
    } else {
        "service"
    });
    let retired = temp.path().join("retired-service");
    std::fs::copy(&source, &installed).unwrap();
    let mut command = Command::new(&installed);
    command
        .args(["serve", "--listen", "127.0.0.1:0", "--workspace"])
        .arg(temp.path().join("workspace"))
        .env("DAAT_LOCUS_HOME", &root)
        .env_remove("DAAT_LOCUS_PROFILE_NAME")
        .env_remove("DAAT_LOCUS_ACCOUNT_STORAGE_ROOT")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut service = Service(command.spawn().unwrap());
    let before = tokio::time::timeout(Duration::from_secs(20), control::wait_ready(&root))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(before.pid, service.0.id());

    // MSI moves the live image to its rollback directory and writes a new file at the old path.
    std::fs::rename(&installed, &retired).unwrap();
    std::fs::copy(&source, &installed).unwrap();
    let after = control::query(&root, Action::Status)
        .await
        .unwrap()
        .expect("the original process must remain discoverable after its image is renamed");
    assert_eq!(after.pid, before.pid);
    assert_eq!(after.instance, before.instance);
    assert_eq!(after.executable, before.executable);

    let metadata = root.join("runtime/daemon.json");
    let mut stale = before.clone();
    stale.process_started += 1;
    std::fs::write(&metadata, serde_json::to_vec(&stale).unwrap()).unwrap();
    assert!(
        control::load(&root).unwrap().is_none(),
        "PID reuse must still fail closed"
    );
    std::fs::write(&metadata, serde_json::to_vec(&before).unwrap()).unwrap();

    control::query(&root, Action::Stop).await.unwrap().unwrap();
    // Reap the child as it exits, including on Unix where an unreaped zombie
    // still has a process-table entry.
    let exit = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Some(exit) = service.0.try_wait().unwrap() {
                break exit;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the renamed service must shut down normally");
    assert!(exit.success(), "service exited with {exit}");
    assert!(control::load(&root).unwrap().is_none());
}
