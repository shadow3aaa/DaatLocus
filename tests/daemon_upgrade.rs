//! A running service must remain controllable when an installer replaces its executable.
use daat_locus::daemon::control::{self, Action};
use std::{
    io::Write,
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

    let cli = |action: &str| {
        let mut command = tokio::process::Command::new(&installed);
        command
            .arg(action)
            .env("DAAT_LOCUS_HOME", &root)
            .env_remove("DAAT_LOCUS_PROFILE_NAME")
            .stdin(Stdio::null())
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x08000000);
        command
    };
    let unchanged = cli("start").output().await.unwrap();
    assert!(unchanged.status.success());
    assert_eq!(
        control::query(&root, Action::Status)
            .await
            .unwrap()
            .unwrap()
            .instance,
        before.instance,
        "starting the same build must leave the independent service running"
    );

    // An executable overlay changes the installed build's hash while retaining
    // an executable image and the exact same semantic package version.
    std::fs::OpenOptions::new()
        .append(true)
        .open(&installed)
        .unwrap()
        .write_all(b"daat-locus-upgrade-regression")
        .unwrap();
    let upgrade = cli("start").output();
    // Reap the child as it exits, including on Unix where an unreaped zombie
    // still has a process-table entry.
    let reaped = async {
        loop {
            if let Some(exit) = service.0.try_wait().unwrap() {
                break exit;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    };
    let (upgrade, exit) = tokio::time::timeout(Duration::from_secs(40), async {
        tokio::join!(upgrade, reaped)
    })
    .await
    .expect("the renamed service must shut down normally");
    assert!(exit.success(), "service exited with {exit}");
    let upgrade = upgrade.unwrap();
    assert!(
        upgrade.status.success(),
        "{}",
        String::from_utf8_lossy(&upgrade.stderr)
    );
    let upgraded = control::query(&root, Action::Status)
        .await
        .unwrap()
        .unwrap();
    // Guard the detached child even if a later assertion fails.
    struct Detached(control::Status);
    impl Drop for Detached {
        fn drop(&mut self) {
            let system = sysinfo::System::new_all();
            if let Some(process) = system.process(sysinfo::Pid::from_u32(self.0.pid))
                && process.start_time() == self.0.process_started
            {
                process.kill();
            }
        }
    }
    let _detached = Detached(upgraded.clone());
    assert_ne!(upgraded.instance, before.instance);
    assert_ne!(upgraded.executable_sha256, before.executable_sha256);
    assert_eq!(upgraded.options.listen, before.options.listen);
    assert_eq!(upgraded.options.workspace, before.options.workspace);
    assert_eq!(upgraded.options.profile, before.options.profile);
    assert!(cli("start").output().await.unwrap().status.success());
    assert_eq!(
        control::query(&root, Action::Status)
            .await
            .unwrap()
            .unwrap()
            .instance,
        upgraded.instance
    );
    control::stop(&root).await.unwrap();
    assert!(control::load(&root).unwrap().is_none());
}
