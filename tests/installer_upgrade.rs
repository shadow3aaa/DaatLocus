#![cfg(windows)]
//! Real owner-local installer coordination, including native display handle release.
use daat_locus::daemon::{control, installation};
use daat_locus_virtual_workspace::displays;
use std::{
    path::Path,
    process::{Child, Command, Stdio},
    time::Duration,
};
use uuid::Uuid;

struct Service(Child);
impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn command(binary: &Path, namespace: &Path) -> Command {
    use std::os::windows::process::CommandExt;
    let mut command = Command::new(binary);
    command
        .env("DAAT_LOCUS_HOME", namespace)
        .env_remove("DAAT_LOCUS_PROFILE_NAME")
        .env_remove("DAAT_LOCUS_ACCOUNT_STORAGE_ROOT")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(0x08000000);
    command
}
struct Restored(control::Status);
impl Drop for Restored {
    fn drop(&mut self) {
        let system = sysinfo::System::new_all();
        if let Some(process) = system.process(sysinfo::Pid::from_u32(self.0.pid))
            && process.start_time() == self.0.process_started
        {
            process.kill();
        }
    }
}

#[tokio::test]
async fn installer_fences_startup_and_restores_only_running_profiles() {
    lifecycle(false).await;
}

#[tokio::test]
#[ignore = "requires signed IDD and an unlocked stationary desktop; run native tests serially"]
async fn installer_releases_the_display_fences_startup_and_restores_only_running_profiles() {
    lifecycle(true).await;
}

async fn lifecycle(native: bool) {
    let temp = tempfile::tempdir().unwrap();
    let namespace = temp.path().join("account");
    let profile = namespace.join("profiles/working");
    let installed = temp.path().join("installation");
    std::fs::create_dir(&installed).unwrap();
    let binary = installed.join("daat-locus.exe");
    std::fs::copy(env!("CARGO_BIN_EXE_daat-locus"), &binary).unwrap();
    std::fs::create_dir_all(namespace.join("profiles/never-started")).unwrap();
    let before = native.then(|| displays().unwrap());
    let mut service = Service(
        command(&binary, &namespace)
            .args([
                "serve",
                "--profile",
                "working",
                "--listen",
                "127.0.0.1:0",
                "--workspace",
            ])
            .arg(temp.path().join("workspace"))
            .spawn()
            .unwrap(),
    );
    let old = control::wait_ready(&profile).await.unwrap();
    let output =
        before.map(|before| {
            displays().unwrap().into_iter()
        .find(|display| !before.iter().any(|item| item.id == display.id))
        .expect("signed IDD must provide a virtual display before the test can verify its release")
        });
    let mut transaction = Uuid::new_v4();
    installation::prepare(&namespace, &installed, transaction)
        .await
        .unwrap();
    assert!(service.0.wait().unwrap().success());
    assert!(control::load(&profile).unwrap().is_none());
    if let Some(output) = output {
        assert!(
            !displays()
                .unwrap()
                .iter()
                .any(|display| display.id == output.id)
        );
    }
    for action in ["start", "serve"] {
        let mut command = tokio::process::Command::from(command(&binary, &namespace));
        command
            .args([action, "--profile", "working"])
            .kill_on_drop(true);
        let status = tokio::time::timeout(Duration::from_secs(10), command.status())
            .await
            .expect("startup must fail promptly while setup is active")
            .unwrap();
        assert!(!status.success());
    }
    assert!(
        installation::resume(&namespace, &installed, Uuid::new_v4())
            .await
            .is_err()
    );
    assert!(
        installation::prepare(&namespace, &installed, Uuid::new_v4())
            .await
            .is_err()
    );
    let foreign = command(&binary, &namespace)
        .args(["installer", "resume", "--installation"])
        .arg(&installed)
        .arg("--transaction")
        .arg(transaction.to_string())
        .status()
        .unwrap();
    assert!(
        !foreign.success(),
        "another live installer cannot finalize this transaction even with the same bundle ID"
    );
    // Simulate a crashed installer/PID generation change, then retry after the
    // old services are already stopped. Their original launch options must survive.
    let journal = namespace.join("runtime/installation.json");
    let mut stale: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&journal).unwrap()).unwrap();
    stale["owner"]["started"] = (stale["owner"]["started"].as_u64().unwrap() + 1).into();
    std::fs::write(&journal, serde_json::to_vec(&stale).unwrap()).unwrap();
    transaction = Uuid::new_v4();
    installation::prepare(&namespace, &installed, transaction)
        .await
        .unwrap();
    // MSI can now replace the image; changing the overlay identifies the new build.
    use std::io::Write;
    std::fs::OpenOptions::new()
        .append(true)
        .open(&binary)
        .unwrap()
        .write_all(b"installer-upgrade-regression")
        .unwrap();
    installation::resume(&namespace, &installed, transaction)
        .await
        .unwrap();
    let current = control::query(&profile, control::Action::Status)
        .await
        .unwrap()
        .unwrap();
    let _restored = Restored(current.clone());
    assert_ne!(current.instance, old.instance);
    assert_ne!(current.executable_sha256, old.executable_sha256);
    assert_eq!(current.options.profile, old.options.profile);
    assert_eq!(current.options.listen, old.options.listen);
    assert_eq!(current.options.workspace, old.options.workspace);
    assert!(control::load(&namespace).unwrap().is_none());
    assert!(
        control::load(&namespace.join("profiles/never-started"))
            .unwrap()
            .is_none()
    );
    assert!(!namespace.join("runtime/installation.json").exists());
    // A repeated rollback/finalization must not restart an already restored service.
    installation::resume(&namespace, &installed, transaction)
        .await
        .unwrap();
    assert_eq!(
        control::load(&profile).unwrap().unwrap().instance,
        current.instance
    );
    tokio::time::timeout(Duration::from_secs(70), control::stop(&profile))
        .await
        .unwrap()
        .unwrap();
}
