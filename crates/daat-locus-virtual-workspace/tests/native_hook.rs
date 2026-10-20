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

#[test]
#[ignore = "requires signed IDD, built hook_lab/Hook DLL, unlocked desktop and stationary cursor"]
fn minimized_window_restores_on_workspace_without_stealing_desktop_input() {
    use daat_locus_virtual_workspace::{DesktopState, SharedDisplay, Window, WindowInput, windows};
    use std::io::{BufRead, BufReader};
    use windows_sys::Win32::{Foundation::HWND, UI::WindowsAndMessaging::*};
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
        "build matching hook_lab and Hook DLL first"
    );
    let display = SharedDisplay::new(Default::default()).unwrap();
    display.initialize().unwrap();
    // IDD arrival is allowed to remap the physical cursor. Finish that setup
    // before measuring window restoration, which must preserve a warm desktop.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut previous = DesktopState::read().unwrap();
    let mut stable = Instant::now();
    while stable.elapsed() < Duration::from_secs(2) {
        assert!(
            Instant::now() < deadline,
            "stationary desktop prerequisite unavailable after display arrival"
        );
        std::thread::sleep(Duration::from_millis(100));
        let current = DesktopState::read().unwrap();
        if current != previous {
            previous = current;
            stable = Instant::now();
        }
    }
    struct Fixture(std::process::Child);
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut fixture = Fixture(
        Command::new(lab)
            .arg("--fixture")
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let stdout = fixture.0.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = tx.send(BufReader::new(stdout).read_line(&mut line).map(|_| line));
    });
    let line = rx.recv_timeout(Duration::from_secs(8)).unwrap().unwrap();
    let handles: Vec<usize> = line
        .trim()
        .strip_prefix("READY ")
        .unwrap()
        .split_whitespace()
        .map(|h| h.parse().unwrap())
        .collect();
    let (agent, edit) = (handles[1], handles[3]);
    let original = Window::inspect(agent).unwrap().bounds;
    assert_ne!(
        unsafe { ShowWindowAsync(agent as HWND, SW_SHOWMINNOACTIVE) },
        0
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    while unsafe { IsIconic(agent as HWND) } == 0 {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    let mut target = windows()
        .unwrap()
        .into_iter()
        .find(|w| w.handle == agent)
        .expect("minimized fixture was omitted");
    assert!(target.minimized);
    let region = display
        .reserve(original.width().unwrap(), original.height().unwrap())
        .unwrap();
    // Minimize animations and display arrival can still change native geometry.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        std::thread::sleep(Duration::from_millis(150));
        let current = Window::inspect(agent).unwrap();
        if current == target {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "minimized fixture did not settle"
        );
        target = current;
    }
    let before = DesktopState::read().unwrap();
    let mut input = WindowInput::attach(&target, dll).unwrap_or_else(|error| {
        panic!(
            "attach minimized: {error}; expected={target:?}; now={:?}",
            Window::inspect(agent)
        )
    });
    assert!(
        input
            .text(edit, "must not be dispatched", Duration::from_secs(2))
            .is_err()
    );
    assert!(Window::inspect(agent).unwrap().minimized);
    let placed = input.place_window(&target, region.bounds()).unwrap_or_else(|error| panic!("restore minimized: {error}; before={before:?}; after={:?}; vetoes={:?}; window={:?}", DesktopState::read(), input.veto_counts(), Window::inspect(agent)));
    assert!(!placed.minimized);
    assert_eq!(placed.bounds, region.bounds());
    assert_eq!(DesktopState::read().unwrap(), before);
    let text = "中文 from minimized";
    input.text(edit, text, Duration::from_secs(2)).unwrap();
    let mut buffer = [0u16; 128];
    let mut length = 0usize;
    assert_ne!(
        unsafe {
            SendMessageTimeoutW(
                edit as HWND,
                WM_GETTEXT,
                buffer.len(),
                buffer.as_mut_ptr() as isize,
                SMTO_ABORTIFHUNG | SMTO_BLOCK,
                2000,
                &mut length,
            )
        },
        0
    );
    assert_eq!(String::from_utf16_lossy(&buffer[..length]), text);
    assert_eq!(DesktopState::read().unwrap(), before);
    input.stop().unwrap();
    let returned = Window::inspect(agent)
        .unwrap()
        .relocate(original)
        .unwrap()
        .minimize()
        .unwrap();
    assert!(returned.minimized);
    assert_eq!(returned.restored_bounds().unwrap(), original);
    assert_eq!(DesktopState::read().unwrap(), before);
}
