use super::*;

#[test]
fn packaged_activation_cannot_claim_another_app_from_the_same_broker_pid() {
    let receipt = LaunchReceipt {
        process_id: Some(42),
        process_started: Some(100),
    };
    assert!(!matches_launch(
        "shell:AppsFolder\\Requested.Package!App",
        42,
        100,
        &receipt,
        |application| application == "Different.Package!App"
    ));
    let no_pid = LaunchReceipt {
        process_id: None,
        process_started: None,
    };
    assert!(matches_launch(
        "shell:AppsFolder\\Requested.Package!App",
        42,
        100,
        &no_pid,
        |application| application == "Requested.Package!App"
    ));
    assert!(!matches_launch(
        "C:\\fixture.exe",
        42,
        101,
        &receipt,
        |_| panic!("ordinary EXEs use process birth identity")
    ));
}

#[test]
#[ignore = "requires signed IDD, built daat-locus binary, launch_target example and Hook DLL; run on an unlocked desktop with stationary cursor, serially"]
fn native_launch_prepares_screen_and_claims_only_the_new_window_with_a_clean_environment() {
    use std::time::Instant;
    let profile = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let example = profile.join("examples/launch_target.exe");
    let launcher = profile.join("daat-locus.exe");
    let dll = profile.join("daat_locus_workspace_hook.dll");
    assert!(
        example.is_file() && launcher.is_file() && dll.is_file(),
        "build the current CLI, launch_target and Hook DLL first"
    );
    let temporary = tempfile::tempdir().unwrap();
    let fixture = temporary.path().join("启动 target.exe");
    std::fs::copy(&example, &fixture).unwrap();
    let fixture_identity = std::fs::canonicalize(&fixture).unwrap();
    struct Existing(std::process::Child);
    impl Drop for Existing {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let existing = Existing(
        Command::new(&fixture)
            .creation_flags(0x0800_0000)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    let original = loop {
        if let Some(window) = daat_locus_virtual_workspace::windows()
            .unwrap()
            .into_iter()
            .find(|w| w.process_id == existing.0.id())
        {
            break window;
        }
        assert!(
            Instant::now() < deadline,
            "existing disposable window unavailable"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    let display = SharedDisplay::new(Default::default()).unwrap();
    let warm = display.reserve(960, 720).unwrap();
    let screen = warm.display().clone();
    drop(warm);
    let baseline = DesktopState::read().unwrap();
    let mut host = Host::new("launch-test".into(), display);
    host.launcher_exe = Some(launcher);
    host.hook_dll = dll;
    host.protected_env = vec!["PATH".into()];
    let app = host.register_app("launch target".into(), fixture.to_str().unwrap().into());
    let cancelled = AtomicBool::new(false);
    let reply = host.launch(&app, &cancelled).unwrap();
    // Always clean our launched fixture even when an assertion below fails. A
    // verified process handle fences PID reuse; no other application is killed.
    let current = host
        .taken
        .values()
        .next()
        .map(|t| t.window.clone())
        .or_else(|| {
            host.candidates
                .values()
                .find(|c| {
                    c.window.process_id != original.process_id
                        && process_path(c.window.process_id).is_ok_and(|p| p == fixture_identity)
                })
                .map(|c| c.window.clone())
        });
    struct Launched(std::os::windows::io::OwnedHandle);
    impl Drop for Launched {
        fn drop(&mut self) {
            use std::os::windows::io::AsRawHandle;
            unsafe {
                windows_sys::Win32::System::Threading::TerminateProcess(self.0.as_raw_handle(), 0);
                windows_sys::Win32::System::Threading::WaitForSingleObject(
                    self.0.as_raw_handle(),
                    2000,
                );
            }
        }
    }
    let own_fixture = |w: &Window| {
        use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
        use windows_sys::Win32::System::Threading::*;
        let raw = unsafe {
            OpenProcess(
                PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                0,
                w.process_id,
            )
        };
        assert!(!raw.is_null());
        let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
        let mut times = [windows_sys::Win32::Foundation::FILETIME::default(); 4];
        let [created, exited, kernel, user] = &mut times;
        assert_ne!(
            unsafe { GetProcessTimes(handle.as_raw_handle(), created, exited, kernel, user) },
            0
        );
        assert_eq!(
            (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime),
            w.process_started
        );
        Launched(handle)
    };
    let _launched = current.as_ref().map(&own_fixture);
    assert_eq!(reply.payload["status"], "observed", "{}", reply.payload);
    assert_eq!(
        reply.payload["startup_placement"],
        "observed_on_virtual_screen"
    );
    assert!(reply.image.is_some());
    assert_eq!(host.taken.len(), 1);
    let target = &host.taken.values().next().unwrap().window;
    assert_ne!(target.process_id, original.process_id);
    assert!(screen.bounds.contains_rect(target.bounds));
    let send = |message: u32, w: usize| {
        let mut value = 0;
        assert_ne!(
            unsafe {
                SendMessageTimeoutW(
                    target.handle as HWND,
                    message,
                    w,
                    0,
                    SMTO_ABORTIFHUNG | SMTO_BLOCK,
                    1000,
                    &mut value,
                )
            },
            0
        );
        value
    };
    assert_eq!(
        send(WM_APP + 74, 0),
        0,
        "protected PATH leaked into the application"
    );
    assert_eq!(
        send(WM_APP + 75, 0),
        1,
        "normal Windows environment was removed"
    );
    assert_eq!(DesktopState::read().unwrap(), baseline);
    original.validate().unwrap();
    assert!(
        host.candidates
            .values()
            .any(|c| c.window.handle == original.handle && !host.taken.contains_key(&c.id))
    );
    host.release_all().unwrap();
    let ignored = temporary.path().join("ignore-position.exe");
    std::fs::copy(&example, &ignored).unwrap();
    let ignored_identity = std::fs::canonicalize(&ignored).unwrap();
    let app = host.register_app(
        "ignores startup placement".into(),
        ignored.to_str().unwrap().into(),
    );
    let baseline = DesktopState::read().unwrap();
    let reply = host.launch(&app, &cancelled).unwrap();
    let ignored_window = host
        .candidates
        .values()
        .find(|c| process_path(c.window.process_id).is_ok_and(|p| p == ignored_identity))
        .map(|c| c.window.clone())
        .expect("new disposable window must remain discoverable after ignored placement");
    let _ignored = own_fixture(&ignored_window);
    assert_eq!(reply.payload["status"], "outcome_unknown");
    assert_eq!(
        reply.payload["startup_placement"],
        "application_ignored_placement"
    );
    assert!(
        reply.image.is_none() && host.taken.is_empty(),
        "ignored hints were reported as a successful virtual-screen launch"
    );
    assert!(
        !screen.bounds.contains_rect(ignored_window.bounds),
        "host silently relocated the target after first showing it on the physical screen"
    );
    assert_eq!(DesktopState::read().unwrap(), baseline);
    original.validate().unwrap();
}
