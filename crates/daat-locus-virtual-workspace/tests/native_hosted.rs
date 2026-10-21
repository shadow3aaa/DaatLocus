#![cfg(windows)]

use daat_locus_virtual_workspace::{
    DesktopState, InputBackend, InputSurface, MouseAction, Point, VirtualWorkspace, Window,
    WindowInput,
};
use std::{
    cell::Cell,
    error::Error,
    io::{BufRead, BufReader, Write},
    process::{Child, Command, Stdio},
    ptr,
    time::Duration,
};
use windows_sys::Win32::{
    Foundation::*,
    System::LibraryLoader::GetModuleHandleW,
    UI::{HiDpi::*, WindowsAndMessaging::*},
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;
const ROLE: &str = "DAAT_LOCUS_HOSTED_WINDOW_TEST_ROLE";
const QUERY: u32 = WM_APP + 50;
const TEST: &str = "cross_process_descendants_rebind_and_revoke_without_authorizing_siblings";
thread_local! { static CLICKS: Cell<u32> = const { Cell::new(0) }; }

unsafe extern "system" fn procedure(hwnd: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match message {
        WM_LBUTTONUP => {
            CLICKS.set(CLICKS.get() + 1);
            return 0;
        }
        WM_COMMAND if w >> 16 == 0 => {
            CLICKS.set(CLICKS.get() + 1);
            return 0;
        }
        QUERY => return CLICKS.get() as isize,
        WM_CLOSE => {
            unsafe {
                DestroyWindow(hwnd);
                PostQuitMessage(0);
            }
            return 0;
        }
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, message, w, l) }
}
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn fixture(role: &str) -> Result<()> {
    unsafe {
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let class = wide("DaatLocusHostedInputTest");
    let module = unsafe { GetModuleHandleW(ptr::null()) };
    let definition = WNDCLASSW {
        lpfnWndProc: Some(procedure),
        hInstance: module,
        lpszClassName: class.as_ptr(),
        ..Default::default()
    };
    assert_ne!(unsafe { RegisterClassW(&definition) }, 0);
    let parent: usize = std::env::var("DAAT_LOCUS_HOSTED_WINDOW_TEST_PARENT")?.parse()?;
    let content = role.ends_with("content");
    if role == "restricted-content" {
        let policy = 1u32; // MicrosoftSignedOnly: our Hook DLL cannot enter this child.
        assert_ne!(
            unsafe {
                windows_sys::Win32::System::Threading::SetProcessMitigationPolicy(
                    windows_sys::Win32::System::Threading::ProcessSignaturePolicy,
                    (&policy as *const u32).cast(),
                    std::mem::size_of_val(&policy),
                )
            },
            0
        );
    }
    let hwnd = unsafe {
        CreateWindowExW(
            if role == "frame" { WS_EX_NOACTIVATE } else { 0 },
            class.as_ptr(),
            wide(role).as_ptr(),
            if content {
                WS_CHILD
            } else {
                WS_OVERLAPPEDWINDOW
            },
            if content { 15 } else { parent as i32 },
            40,
            420,
            400,
            if content {
                parent as HWND
            } else {
                ptr::null_mut()
            },
            ptr::null_mut(),
            module,
            ptr::null(),
        )
    };
    assert!(!hwnd.is_null());
    let edit = if content {
        unsafe {
            CreateWindowExW(
                0,
                wide("EDIT").as_ptr(),
                wide("").as_ptr(),
                WS_CHILD | WS_VISIBLE | 0x80,
                20,
                20,
                240,
                35,
                hwnd,
                ptr::null_mut(),
                module,
                ptr::null(),
            )
        }
    } else {
        ptr::null_mut()
    };
    let button = if content {
        unsafe {
            CreateWindowExW(
                0,
                wide("BUTTON").as_ptr(),
                wide("Native action").as_ptr(),
                WS_CHILD | WS_VISIBLE,
                20,
                100,
                160,
                40,
                hwnd,
                10usize as _,
                module,
                ptr::null(),
            )
        }
    } else {
        ptr::null_mut()
    };
    if role == "restricted-content" {
        // Like Chromium's compositor pane: visible, disabled, overlapping live
        // controls, and earlier in the raw UIA sibling order.
        assert!(
            !unsafe {
                CreateWindowExW(
                    WS_EX_TRANSPARENT,
                    wide("STATIC").as_ptr(),
                    wide("decoration").as_ptr(),
                    WS_CHILD | WS_VISIBLE | WS_DISABLED,
                    0,
                    0,
                    420,
                    400,
                    hwnd,
                    ptr::null_mut(),
                    module,
                    ptr::null(),
                )
            }
            .is_null()
        );
    }
    unsafe {
        ShowWindow(
            hwnd,
            if role == "human" {
                SW_SHOW
            } else {
                SW_SHOWNOACTIVATE
            },
        );
        if role == "human" {
            SetForegroundWindow(hwnd);
        }
    }
    println!(
        "HOSTED_READY {} {} {}",
        hwnd as usize, edit as usize, button as usize
    );
    std::io::stdout().flush()?;
    let mut message = MSG::default();
    while unsafe { GetMessageW(&mut message, ptr::null_mut(), 0, 0) } > 0 {
        unsafe {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    Ok(())
}

struct Fixture {
    child: Child,
    hwnd: usize,
    edit: usize,
    button: usize,
}
impl Fixture {
    fn start(role: &str, parent: usize) -> Result<Self> {
        Self::start_for(role, parent, TEST)
    }
    fn start_for(role: &str, parent: usize, test: &str) -> Result<Self> {
        let mut child = Command::new(std::env::current_exe()?)
            .args([
                "--ignored",
                "--exact",
                test,
                "--nocapture",
                "--test-threads=1",
            ])
            .env(ROLE, role)
            .env("DAAT_LOCUS_HOSTED_WINDOW_TEST_PARENT", parent.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let stdout = child.stdout.take().ok_or("child stdout missing")?;
        let (send, receive) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().take(12) {
                if let Ok(line) = line
                    && let Some(ready) = line.split("HOSTED_READY ").nth(1)
                {
                    let _ = send.send(ready.to_owned());
                    break;
                }
            }
        });
        let mut fixture = Self {
            child,
            hwnd: 0,
            edit: 0,
            button: 0,
        };
        let ready = receive.recv_timeout(Duration::from_secs(8))?;
        let handles: Vec<usize> = ready
            .split_whitespace()
            .map(str::parse)
            .collect::<std::result::Result<_, _>>()?;
        fixture.hwnd = handles[0];
        fixture.edit = handles[1];
        fixture.button = handles[2];
        Ok(fixture)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn send(hwnd: usize, message: u32, w: usize, l: isize) -> Result<usize> {
    let mut value = 0;
    if unsafe {
        SendMessageTimeoutW(
            hwnd as _,
            message,
            w,
            l,
            SMTO_ABORTIFHUNG | SMTO_BLOCK,
            2000,
            &mut value,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(value)
}
fn text(hwnd: usize) -> Result<String> {
    let mut text = [0u16; 256];
    let length = send(hwnd, WM_GETTEXT, text.len(), text.as_mut_ptr() as isize)?;
    Ok(String::from_utf16(&text[..length])?)
}

#[test]
#[ignore = "requires matching built Hook DLL on an unlocked interactive desktop; no IDD required"]
fn restricted_child_keeps_native_routes_and_skips_disabled_uia_decoration() -> Result<()> {
    const CASE: &str = "restricted_child_keeps_native_routes_and_skips_disabled_uia_decoration";
    if let Ok(role) = std::env::var(ROLE) {
        return fixture(&role);
    }
    unsafe {
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let frame = Fixture::start_for("frame", 80, CASE)?;
    let native = Fixture::start_for("content", frame.hwnd, CASE)?;
    let restricted = Fixture::start_for("restricted-content", frame.hwnd, CASE)?;
    let target = Window::inspect(frame.hwnd)?;
    let dll = std::env::current_exe()?
        .parent()
        .ok_or("test directory missing")?
        .parent()
        .ok_or("profile missing")?
        .join("daat_locus_workspace_hook.dll");
    assert!(dll.is_file(), "build daat-locus-workspace-hook first");
    let mut input = WindowInput::attach(&target, &dll)?;
    assert_eq!(input.backend_for(native.edit)?, InputBackend::Hook);
    assert_eq!(
        input.backend_for(restricted.button)?,
        InputBackend::Automation
    );
    assert!(
        input
            .supported_actions_for(native.edit)?
            .contains(&"literal_unicode")
    );
    assert!(
        !input
            .supported_actions_for(restricted.button)?
            .contains(&"literal_unicode")
    );
    input.text(native.edit, "仍然支持中文", Duration::from_secs(3))?;
    assert_eq!(text(native.edit)?, "仍然支持中文");
    input.mouse(
        restricted.hwnd,
        MouseAction::ClickLeft,
        Point { x: 45, y: 115 },
        Duration::from_secs(3),
    )?;
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while send(restricted.hwnd, QUERY, 0, 0)? == 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        send(restricted.hwnd, QUERY, 0, 0)?,
        1,
        "UIA must invoke the live button exactly once through the decorative sibling"
    );
    unsafe {
        windows_sys::Win32::UI::Input::KeyboardAndMouse::EnableWindow(restricted.button as _, 0);
    }
    assert!(
        input
            .mouse(
                restricted.button,
                MouseAction::ClickLeft,
                Point { x: 25, y: 15 },
                Duration::from_secs(3)
            )
            .is_err()
    );
    assert_eq!(
        send(restricted.hwnd, QUERY, 0, 0)?,
        1,
        "a disabled actionable button must remain blocked"
    );
    assert!(
        input.input_enabled(),
        "pre-dispatch refusal must preserve the lease"
    );
    input.text(native.edit, "，继续", Duration::from_secs(3))?;
    assert_eq!(text(native.edit)?, "仍然支持中文，继续");
    input.stop()?;
    assert!(
        input
            .text(native.edit, "revoked", Duration::from_secs(3))
            .is_err()
    );
    assert_eq!(text(native.edit)?, "仍然支持中文，继续");
    Ok(())
}

#[test]
#[ignore = "requires installed signed IDD, matching built Hook DLL and stationary cursor on an unlocked interactive desktop"]
fn cross_process_descendants_rebind_and_revoke_without_authorizing_siblings() -> Result<()> {
    if let Ok(role) = std::env::var(ROLE) {
        return fixture(&role);
    }
    unsafe {
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let workspace = VirtualWorkspace::create(Default::default())?;
    let physical = daat_locus_virtual_workspace::displays()?
        .into_iter()
        .find(|d| !d.indirect && !d.cloned)
        .ok_or("physical output missing")?;
    let human = Fixture::start("human", (physical.bounds.left + 40) as usize)?;
    let area = workspace.display().bounds;
    let frame = Fixture::start("frame", (area.left + 20) as usize)?;
    let sibling = Fixture::start("frame", (area.left + 800) as usize)?;
    let content = Fixture::start("content", frame.hwnd)?;
    let target = Window::inspect(frame.hwnd)?;
    assert!(target.owns_control(content.edit));
    assert!(!target.owns_control(sibling.hwnd));
    let baseline = DesktopState::read()?;
    assert_ne!(baseline.foreground, frame.hwnd);
    let dll = std::env::current_exe()?
        .parent()
        .ok_or("test directory missing")?
        .parent()
        .ok_or("profile missing")?
        .join("daat_locus_workspace_hook.dll");
    let mut hook = workspace.hook(&target, &dll)?;
    hook.text(content.edit, "中文 first", Duration::from_secs(3))?;
    assert_eq!(text(content.edit)?, "中文 first");
    hook.mouse(
        content.hwnd,
        MouseAction::ClickLeft,
        Point { x: 300, y: 100 },
        Duration::from_secs(3),
    )?;
    assert_eq!(send(content.hwnd, QUERY, 0, 0)?, 1);
    assert!(
        hook.text(sibling.hwnd, "wrong window", Duration::from_secs(3))
            .is_err()
    );
    assert!(
        hook.input_enabled(),
        "rejecting an unrelated sibling must not poison a valid route"
    );
    let old = content.edit;
    let old_surface = InputSurface::inspect(&target, old)?;
    drop(content);
    // Retire a dead content process and discover a new process under the same frame.
    let replacement = Fixture::start("content", frame.hwnd)?;
    hook.reobserve()?;
    assert!(
        old_surface.validate(&target).is_err(),
        "old content identity survived process replacement"
    );
    if !target.owns_control(old) {
        assert!(hook.text(old, "stale", Duration::from_secs(3)).is_err());
    }
    hook.text(replacement.edit, "新 content", Duration::from_secs(3))?;
    assert_eq!(text(replacement.edit)?, "新 content");
    assert_eq!(DesktopState::read()?, baseline);
    hook.stop()?;
    assert!(
        hook.text(replacement.edit, "revoked", Duration::from_secs(3))
            .is_err()
    );
    assert!(
        hook.mouse(
            frame.hwnd,
            MouseAction::ClickLeft,
            Point { x: 30, y: 100 },
            Duration::from_secs(3)
        )
        .is_err()
    );
    assert_eq!(text(replacement.edit)?, "新 content");
    assert_eq!(DesktopState::read()?, baseline);
    drop(hook);
    // Exercise accessibility via the same production input abstraction on an
    // independent real native BUTTON, without the test invoking UIA itself.
    let mut automation = WindowInput::attach_automation(&target, &dll)?;
    assert_eq!(automation.backend(), InputBackend::Automation);
    let mut button_bounds = RECT::default();
    assert_ne!(
        unsafe { GetWindowRect(replacement.button as _, &mut button_bounds) },
        0
    );
    let (control, local) = target.input_at(Point {
        x: button_bounds.left + 25 - target.client_bounds.left,
        y: button_bounds.top + 15 - target.client_bounds.top,
    })?;
    let receipt = automation.mouse(
        control,
        MouseAction::ClickLeft,
        local,
        Duration::from_secs(3),
    )?;
    assert!(
        !receipt.desktop.focus_changed()
            && !receipt.desktop.cursor_changed()
            && !receipt.desktop.capture_changed()
    );
    assert_eq!(send(replacement.hwnd, QUERY, 0, 0)?, 1);
    assert!(automation.prepare(sibling.hwnd).is_err());
    automation.stop()?;
    assert!(
        automation
            .mouse(
                control,
                MouseAction::ClickLeft,
                local,
                Duration::from_secs(3)
            )
            .is_err()
    );
    assert_eq!(send(replacement.hwnd, QUERY, 0, 0)?, 1);
    assert_eq!(DesktopState::read()?, baseline);
    drop(automation);
    drop(replacement);
    drop(sibling);
    drop(frame);
    drop(human);
    workspace.close()?;
    Ok(())
}
