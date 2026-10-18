#![cfg(windows)]

use std::{
    process::{Command, Stdio},
    time::{Duration, Instant},
};

fn run(owned: bool) {
    let profile = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let lab = profile.join("examples/hook_lab.exe");
    let dll = profile.join("daat_locus_workspace_hook.dll");
    assert!(
        lab.is_file() && dll.is_file(),
        "build daat-locus-workspace-hook and the hook_lab example in the same profile first"
    );
    let mut command = Command::new(lab);
    command
        .arg("--dll")
        .arg(dll)
        .arg("--self-test")
        .stdin(Stdio::null());
    if owned {
        command.arg("--owned-display");
    } else {
        command.arg("--physical-screen");
    }
    let mut child = command.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "native hook lab failed: {status}");
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("native hook lab timed out");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
#[ignore = "requires matching built Hook DLL/example, interactive foreground terminal and stationary cursor"]
fn external_same_thread_windows_preserve_human_input_and_hook_lifecycle() {
    run(false);
}

#[test]
#[ignore = "requires signed IDD driver, built Hook DLL/example, interactive foreground terminal and stationary cursor"]
fn owned_display_capture_and_hook_input_preserve_human_window() {
    run(true);
}
