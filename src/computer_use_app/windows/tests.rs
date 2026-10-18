use super::*;

#[test]
#[ignore = "requires installed signed IDD, built hook_lab/Hook DLL, unlocked desktop and stationary cursor"]
fn native_actor_take_input_give_revocation_and_inflight_handoff() {
    use std::{
        io::{BufRead, BufReader},
        process::{Command, Stdio},
        time::Instant,
    };
    unsafe {
        windows_sys::Win32::UI::HiDpi::SetThreadDpiAwarenessContext(
            windows_sys::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        );
    }
    let profile = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let example = profile.join("examples/hook_lab.exe");
    let dll = profile.join("daat_locus_workspace_hook.dll");
    assert!(
        example.is_file() && dll.is_file(),
        "build hook_lab and the matching Hook DLL first"
    );
    struct Fixture(std::process::Child);
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut fixture = Fixture(
        Command::new(example)
            .args(["--fixture", "--agent-x", "550", "--agent-y", "200"])
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let stdout = fixture.0.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let result = BufReader::new(stdout).read_line(&mut line).map(|_| line);
        let _ = tx.send(result);
    });
    let line = rx.recv_timeout(Duration::from_secs(8)).unwrap().unwrap();
    let handles: Vec<usize> = line
        .trim()
        .strip_prefix("READY ")
        .unwrap()
        .split_whitespace()
        .map(|p| p.parse().unwrap())
        .collect();
    let (human, agent, edit, button) = (handles[0], handles[1], handles[3], handles[4]);
    let send = |hwnd: usize, msg: u32, w: usize, l: isize| -> usize {
        let mut result = 0;
        assert_ne!(
            unsafe {
                SendMessageTimeoutW(
                    hwnd as HWND,
                    msg,
                    w,
                    l,
                    SMTO_ABORTIFHUNG | SMTO_BLOCK,
                    2000,
                    &mut result,
                )
            },
            0
        );
        result
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    while unsafe { GetForegroundWindow() } as usize != human {
        send(human, WM_APP + 41, 0, 0);
        assert!(
            Instant::now() < deadline,
            "foreground fixture prerequisite denied"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let before = DesktopState::read().unwrap();
    let display = SharedDisplay::new(Default::default()).unwrap();
    let mut host = Host::new("native-actor".into(), display.clone());
    host.hook_dll = dll.clone();
    host.discover().unwrap();
    let window_id = host
        .candidates
        .values()
        .find(|c| c.window.handle == agent)
        .unwrap()
        .id
        .clone();
    let mut context = AppToolExecutionContext {
        execution_cwd: profile.clone(),
        sandbox_policy: crate::sandbox::RuntimeSandboxPolicy::disabled(),
        tool_output_max_tokens: 4000,
        turn_epoch: 1,
        actor_call: None,
    };
    context.sandbox_policy.filesystem.full_disk_read = false;
    context.sandbox_policy.filesystem.full_disk_write = false;
    context.sandbox_policy.strong_filesystem =
        crate::sandbox::StrongFilesystemSandboxMode::Required;
    ComputerUseApp::check_context(&context).unwrap();
    let cancelled = AtomicBool::new(false);
    let call = |name: &str, arguments: Value| AgentToolCall {
        id: id(),
        name: name.into(),
        arguments,
    };
    let settle = |host: &mut Host, window_id: &str| {
        let deadline = Instant::now() + Duration::from_secs(4);
        loop {
            let observed = match host.observe(window_id) {
                Ok(observed) => observed,
                Err(error) => {
                    assert!(
                        Instant::now() < deadline,
                        "fixture observation did not settle: {error}"
                    );
                    std::thread::sleep(Duration::from_millis(50));
                    continue;
                }
            };
            let geometry = host.taken[window_id].window.clone();
            // This fixture crosses physical/IDD DPI domains. Wait for its native
            // layout before testing input; stale production images remain refused.
            std::thread::sleep(Duration::from_millis(150));
            if geometry.validate().is_ok() {
                return observed;
            }
            assert!(
                Instant::now() < deadline,
                "fixture DPI layout did not settle"
            );
        }
    };
    let opened = host
        .execute(
            &call("open_window", json!({"window_id":window_id})),
            &context,
            &cancelled,
        )
        .unwrap();
    assert!(
        matches!(
            opened.payload["status"].as_str(),
            Some("observed" | "outcome_unknown")
        ),
        "{}",
        opened.payload
    );
    // A physical/IDD DPI transition may still relayout controls after placement.
    // Recover by observing the same lease; never replay open_window or input.
    let opened = settle(&mut host, &window_id);
    let dimensions = |reply: &Reply| {
        let decoder = png::Decoder::new(std::io::Cursor::new(reply.image.as_ref().unwrap()))
            .read_info()
            .unwrap();
        (decoder.info().width as i32, decoder.info().height as i32)
    };
    let bounds = host.taken[&window_id].window.bounds;
    assert_eq!(
        dimensions(&opened),
        (bounds.width().unwrap(), bounds.height().unwrap())
    );
    assert!(
        bounds.height().unwrap()
            > host.taken[&window_id]
                .window
                .client_bounds
                .height()
                .unwrap(),
        "fixture must include a nonclient title bar"
    );
    let shared_screen = host.workspace.as_ref().unwrap().display().clone();
    let warmed_cursor = DesktopState::read().unwrap().cursor;
    // Even an excluded DL-process window must not enter another window's image.
    let client = host.taken[&window_id].window.bounds;
    let overlay = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            wide("STATIC").as_ptr(),
            wide("capture occluder").as_ptr(),
            WS_POPUP | WS_VISIBLE,
            client.left + 4,
            client.top + 4,
            20,
            8,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null(),
        )
    };
    assert!(!overlay.is_null());
    struct Overlay(HWND);
    impl Drop for Overlay {
        fn drop(&mut self) {
            unsafe {
                DestroyWindow(self.0);
            }
        }
    }
    let overlay = Overlay(overlay);
    let observed_state = host.taken[&window_id].state.as_deref().unwrap();
    assert!(
        host.check_state(&window_id, observed_state).is_err(),
        "input accepted through a new occluder"
    );
    assert!(
        host.observe(&window_id).is_err(),
        "screen capture included an overlapping window"
    );
    assert!(host.taken[&window_id].state.is_none());
    drop(overlay);
    let opened = settle(&mut host, &window_id);
    eprintln!(
        "desktop before take: {before:?}; after take: {:?}",
        DesktopState::read().unwrap()
    );
    let full_state = opened.payload["observation"]["state_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let target = &host.taken[&window_id].window;
    let mut edit_rect = RECT::default();
    unsafe {
        GetWindowRect(edit as HWND, &mut edit_rect);
    }
    let region = ImageRegion {
        x: edit_rect.left - target.bounds.left,
        y: edit_rect.top - target.bounds.top,
        width: edit_rect.right - edit_rect.left,
        height: edit_rect.bottom - edit_rect.top,
    };
    let cropped = host
        .execute(
            &call(
                "get_window_state",
                json!({"window_id":window_id,"region":region,"scale":2}),
            ),
            &context,
            &cancelled,
        )
        .unwrap();
    assert_eq!(dimensions(&cropped), (region.width * 2, region.height * 2));
    assert!(
        host.check_state(&window_id, &full_state).is_err(),
        "full image coordinates survived a new cropped view"
    );
    let state = cropped.payload["observation"]["state_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (x, y) = (16, 16);
    let clicked=host.execute(&call("click",json!({"window_id":window_id,"state_id":state,"x":x,"y":y,"button":"left","click_count":1})),&context,&cancelled).unwrap();
    let state2 = clicked.payload["observation"]["state_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let routed = host.taken[&window_id].receiver.as_ref().unwrap();
    assert_eq!(
        routed.hwnd, edit,
        "image point selected the wrong native control"
    );
    let current_receiver = input_target(&host.taken[&window_id].window, routed.hwnd).unwrap();
    assert_eq!(routed.surface, current_receiver.surface);
    assert!(
        host.taken[&window_id].window.owns_control(routed.hwnd),
        "receiver left the window family"
    );
    assert!(
        same_rect(routed.bounds, current_receiver.bounds),
        "receiver moved: {:?} -> {:?}",
        [
            routed.bounds.left,
            routed.bounds.top,
            routed.bounds.right,
            routed.bounds.bottom
        ],
        [
            current_receiver.bounds.left,
            current_receiver.bounds.top,
            current_receiver.bounds.right,
            current_receiver.bounds.bottom
        ]
    );
    assert!(
        host.execute(
            &call(
                "type_text",
                json!({"window_id":window_id,"state_id":state,"text":"stale"})
            ),
            &context,
            &cancelled
        )
        .is_err()
    );
    let typed = host
        .execute(
            &call(
                "type_text",
                json!({"window_id":window_id,"state_id":state2,"text":"后台窗口中文😀"}),
            ),
            &context,
            &cancelled,
        )
        .unwrap();
    let mut actual = [0u16; 128];
    let length = send(edit, WM_GETTEXT, actual.len(), actual.as_mut_ptr() as isize);
    assert_eq!(
        String::from_utf16(&actual[..length]).unwrap(),
        "后台窗口中文😀"
    );
    let before_adjust = DesktopState::read().unwrap();
    let old_state = typed.payload["observation"]["state_id"]
        .as_str()
        .unwrap()
        .to_owned();
    for (action, width, height, x, y) in [
        ("resize", Some(600), Some(400), None, None),
        ("move", None, None, Some(60), Some(60)),
        ("fit", None, None, None, None),
        ("resize", Some(600), Some(400), None, None),
    ] {
        let state = host.taken[&window_id].state.clone().unwrap();
        let adjusted = host.execute(&call("adjust_window", json!({"window_id":window_id,"state_id":state,"action":action,"width":width,"height":height,"x":x,"y":y})), &context, &cancelled).unwrap();
        if adjusted.payload["status"] != "observed" {
            let mut above = unsafe { GetWindow(agent as HWND, GW_HWNDPREV) };
            let current = Window::inspect(agent).unwrap();
            eprintln!(
                "adjustment geometry: {:?}, reservation: {:?}",
                current.bounds,
                host.taken[&window_id].region.bounds()
            );
            for _ in 0..256 {
                if above.is_null() {
                    break;
                }
                let mut rect = RECT::default();
                unsafe {
                    GetWindowRect(above, &mut rect);
                }
                if unsafe { IsWindowVisible(above) } != 0
                    && rect.left < current.bounds.right
                    && current.bounds.left < rect.right
                    && rect.top < current.bounds.bottom
                    && current.bounds.top < rect.bottom
                {
                    let mut class = [0u16; 256];
                    let length =
                        unsafe { GetClassNameW(above, class.as_mut_ptr(), class.len() as i32) };
                    eprintln!(
                        "overlapping class={} bounds={:?}",
                        String::from_utf16_lossy(&class[..length as usize]),
                        [rect.left, rect.top, rect.right, rect.bottom]
                    );
                }
                above = unsafe { GetWindow(above, GW_HWNDPREV) };
            }
        }
        assert_eq!(
            adjusted.payload["status"], "observed",
            "{action}: {}",
            adjusted.payload
        );
        let current = Window::inspect(agent).unwrap();
        assert_eq!(
            current.bounds,
            host.taken[&window_id].region.bounds(),
            "action={action}, maximized={}",
            unsafe { IsZoomed(agent as HWND) }
        );
        assert_eq!(
            dimensions(&adjusted),
            (
                current.bounds.width().unwrap(),
                current.bounds.height().unwrap()
            )
        );
        if let (Some(width), Some(height)) = (width, height) {
            assert_eq!(dimensions(&adjusted), (width, height));
        }
        if let (Some(x), Some(y)) = (x, y) {
            assert_eq!(
                (
                    current.bounds.left - shared_screen.bounds.left,
                    current.bounds.top - shared_screen.bounds.top
                ),
                (x, y)
            );
        }
        assert!(host.taken[&window_id].receiver.is_none());
        assert!(host.check_state(&window_id, &old_state).is_err());
        assert_eq!(host.workspace.as_ref().unwrap().display(), &shared_screen);
    }
    // A maximized app can expand beyond its lease. Restore must recover with no
    // usable screenshot/state, while the existing activation guards stay active.
    send(agent, WM_APP + 48, 0, 0);
    assert_ne!(unsafe { IsZoomed(agent as HWND) }, 0);
    assert!(host.observe(&window_id).is_err());
    let restored = host.execute(&call("adjust_window", json!({"window_id":window_id,"state_id":null,"action":"restore","width":null,"height":null,"x":null,"y":null})), &context, &cancelled).unwrap();
    assert_eq!(
        restored.payload["status"], "observed",
        "{}",
        restored.payload
    );
    assert_eq!(unsafe { IsZoomed(agent as HWND) }, 0);
    assert_eq!(dimensions(&restored), (600, 400));
    assert_eq!(
        DesktopState::read().unwrap(),
        before_adjust,
        "window management interfered with the human desktop"
    );
    let typed = settle(&mut host, &window_id);
    let mut other = Host::new("other-actor".into(), display.clone());
    other.hook_dll = dll;
    other.discover().unwrap();
    let other_id = other
        .candidates
        .values()
        .find(|c| c.window.handle == agent)
        .unwrap()
        .id
        .clone();
    assert!(
        other.take(&other_id, &cancelled).is_err(),
        "another Actor must not acquire the same GUI thread"
    );
    let last = typed.payload["observation"]["state_id"].as_str().unwrap();
    eprintln!("desktop before give: {:?}", DesktopState::read().unwrap());
    let given = host
        .execute(
            &call(
                "give_window",
                json!({"window_id":window_id,"state_id":last,"placement":"right_half"}),
            ),
            &context,
            &cancelled,
        )
        .unwrap();
    assert_eq!(given.payload["status"], "given_to_user");
    for (name, args) in [
        ("get_window_state", json!({"window_id":window_id})),
        (
            "type_text",
            json!({"window_id":window_id,"state_id":last,"text":"forbidden"}),
        ),
        (
            "close_window",
            json!({"window_id":window_id,"state_id":last}),
        ),
        ("open_window", json!({"window_id":window_id})),
    ] {
        assert!(
            host.execute(&call(name, args), &context, &cancelled)
                .is_err(),
            "old reference accepted: {name}"
        );
    }
    assert!(host.taken.is_empty() && host.hooks.is_empty() && host.workspace.is_none());
    assert!(
        displays().unwrap().contains(&shared_screen),
        "last handoff unplugged the shared display"
    );
    assert_eq!(
        DesktopState::read().unwrap().cursor,
        warmed_cursor,
        "handoff changed topology or moved the cursor"
    );
    let now = Window::inspect(agent).unwrap();
    assert!(
        displays()
            .unwrap()
            .iter()
            .filter(|d| !d.indirect)
            .any(|d| d.bounds.contains_rect(now.bounds))
    );
    let after = DesktopState::read().unwrap();
    assert_eq!(
        (before.foreground, before.focus, before.capture),
        (after.foreground, after.focus, after.capture),
        "handoff changed the human foreground/focus/capture"
    );
    // Initial monitor arrival may recenter the cursor. Warm handoff, reacquisition
    // and App reconstruction must preserve its location instead.
    eprintln!(
        "cursor across first IDD creation: {:?} -> {:?}",
        before.cursor, after.cursor
    );
    host.discover().unwrap();
    let fresh = host
        .candidates
        .values()
        .find(|c| c.window.handle == agent)
        .unwrap()
        .id
        .clone();
    assert_ne!(fresh, window_id);
    host.execute(
        &call("open_window", json!({"window_id":fresh})),
        &context,
        &cancelled,
    )
    .unwrap();
    settle(&mut host, &fresh);
    assert_eq!(host.workspace.as_ref().unwrap().display(), &shared_screen);
    assert_eq!(
        DesktopState::read().unwrap().cursor,
        warmed_cursor,
        "reacquiring a window recreated the display"
    );
    // Native BUTTON blocks inside its real click handler; the command has started,
    // so give_window must revoke input but refuse to move it until that handler ends.
    send(agent, WM_APP + 45, 8000, 0);
    // The physical->IDD DPI transition may finish after open_window's first
    // image. Refresh after the fixture has processed its arming message.
    settle(&mut host, &fresh);
    let target = &host.taken[&fresh].window;
    let mut rect = RECT::default();
    unsafe {
        GetWindowRect(button as HWND, &mut rect);
    }
    let (x, y) = (
        rect.left - target.bounds.left + 12,
        rect.top - target.bounds.top + 12,
    );
    let state = host.taken[&fresh].state.clone().unwrap();
    let stalled=host.execute(&call("click",json!({"window_id":fresh,"state_id":state,"x":x,"y":y,"button":"left","click_count":1})),&context,&cancelled).unwrap();
    assert_eq!(stalled.payload["status"], "outcome_unknown");
    let observed = host.observe(&fresh).unwrap();
    let state = observed.payload["observation"]["state_id"]
        .as_str()
        .unwrap();
    let handing = host
        .execute(
            &call(
                "give_window",
                json!({"window_id":fresh,"state_id":state,"placement":"center"}),
            ),
            &context,
            &cancelled,
        )
        .unwrap();
    assert_eq!(handing.payload["status"], "handoff_pending");
    assert!(host.returning.contains_key(&fresh));
    assert!(!host.taken.contains_key(&fresh));
    let current = Window::inspect(agent).unwrap();
    assert!(
        host.workspace
            .as_ref()
            .unwrap()
            .display()
            .bounds
            .contains_rect(current.bounds),
        "window moved while input was running"
    );
    assert!(
        host.observe(&fresh).is_err(),
        "pending handoff must not regain observation/input authority"
    );
    let finished = host
        .execute(
            &call("release_window", json!({"window_id":fresh})),
            &context,
            &cancelled,
        )
        .unwrap();
    assert_eq!(finished.payload["status"], "given_to_user");
    assert_eq!(
        send(agent, WM_APP + 40, 4, 0),
        1,
        "timed out click replayed"
    );
    context.turn_epoch = 0;
    assert!(
        host.execute(
            &call(
                "list_windows",
                json!({"query":null,"app_id":null,"cursor":null,"limit":10})
            ),
            &context,
            &cancelled
        )
        .is_err()
    );
    host.release_all().unwrap();
    drop(host);
    let mut rebuilt = Host::new("rebuilt-app".into(), display.clone());
    rebuilt.hook_dll = profile.join("daat_locus_workspace_hook.dll");
    rebuilt.discover().unwrap();
    let rebuilt_id = rebuilt
        .candidates
        .values()
        .find(|c| c.window.handle == agent)
        .unwrap()
        .id
        .clone();
    rebuilt
        .execute(
            &call("open_window", json!({"window_id":rebuilt_id})),
            &context,
            &cancelled,
        )
        .unwrap();
    settle(&mut rebuilt, &rebuilt_id);
    assert_eq!(
        rebuilt.workspace.as_ref().unwrap().display(),
        &shared_screen,
        "App reconstruction recreated the display"
    );
    assert_eq!(DesktopState::read().unwrap().cursor, warmed_cursor);
    rebuilt.release_all().unwrap();
}

#[test]
fn captured_crop_encodes_actual_bgra_pixels_in_png_order() {
    let frame = daat_locus_virtual_workspace::Frame {
        bounds: Rect {
            left: -2,
            top: 10,
            right: 0,
            bottom: 12,
        },
        width: 2,
        height: 2,
        bgra: vec![1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255, 10, 11, 12, 255],
    };
    let bytes = crop_png(
        &frame,
        Rect {
            left: -1,
            top: 10,
            right: 0,
            bottom: 12,
        },
    )
    .unwrap();
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes))
        .read_info()
        .unwrap();
    let mut output = vec![0; decoder.output_buffer_size().unwrap()];
    let info = decoder.next_frame(&mut output).unwrap();
    assert_eq!((info.width, info.height), (1, 2));
    assert_eq!(
        &output[..info.buffer_size()],
        &[6, 5, 4, 255, 12, 11, 10, 255]
    );
    let bytes = crop_png(&frame, frame.bounds).unwrap();
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes))
        .read_info()
        .unwrap();
    let mut output = vec![0; decoder.output_buffer_size().unwrap()];
    let info = decoder.next_frame(&mut output).unwrap();
    assert_eq!((info.width, info.height), (2, 2));
    assert_eq!(
        &output[..info.buffer_size()],
        &[3, 2, 1, 255, 6, 5, 4, 255, 9, 8, 7, 255, 12, 11, 10, 255]
    );
    let bytes = crop_png_scaled(
        &frame,
        Rect {
            left: -1,
            top: 11,
            right: 0,
            bottom: 12,
        },
        2,
    )
    .unwrap();
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes))
        .read_info()
        .unwrap();
    let mut output = vec![0; decoder.output_buffer_size().unwrap()];
    let info = decoder.next_frame(&mut output).unwrap();
    assert_eq!((info.width, info.height), (2, 2));
    assert_eq!(&output[..info.buffer_size()], &[12, 11, 10, 255].repeat(4));
    assert!(
        crop_png(
            &frame,
            Rect {
                left: -3,
                top: 10,
                right: 0,
                bottom: 12
            }
        )
        .is_err()
    );
}
