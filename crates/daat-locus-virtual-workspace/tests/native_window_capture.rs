#![cfg(windows)]
//! Driver-independent regression for compositor capture and window-local hit testing.
use daat_locus_virtual_workspace::{DesktopState, Point, Window};
use std::{ptr, sync::mpsc, time::Duration};
use windows_sys::Win32::{
    Foundation::{HWND, POINT},
    Graphics::Gdi::*,
    System::Threading::GetCurrentThreadId,
    UI::{HiDpi::*, Input::KeyboardAndMouse::EnableWindow, WindowsAndMessaging::*},
};

struct Fixture {
    thread: Option<std::thread::JoinHandle<()>>,
    id: u32,
    root: usize,
    button: usize,
}
unsafe extern "system" fn procedure(hwnd: HWND, message: u32, w: usize, l: isize) -> isize {
    if message == WM_WINDOWPOSCHANGING
        && !unsafe { GetPropW(hwnd, windows_sys::w!("dl-test-constrain-size")) }.is_null()
    {
        let position = unsafe { &mut *(l as *mut WINDOWPOS) };
        if position.flags & SWP_NOSIZE == 0 {
            position.cx = position.cx.max(320);
            position.cy = position.cy.max(220);
        }
    }
    if message == WM_WINDOWPOSCHANGING
        && !unsafe { GetPropW(hwnd, windows_sys::w!("dl-test-dpi-shrink")) }.is_null()
    {
        let position = unsafe { &mut *(l as *mut WINDOWPOS) };
        if position.flags & SWP_NOSIZE == 0 {
            position.cx = position.cx * 2 / 3;
            position.cy = position.cy * 2 / 3;
        }
    }
    unsafe { DefWindowProcW(hwnd, message, w, l) }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        unsafe {
            PostThreadMessageW(self.id, WM_QUIT, 0, 0);
        }
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}
fn fixture() -> Fixture {
    let (tx, rx) = mpsc::sync_channel(1);
    let thread = std::thread::spawn(move || unsafe {
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let class: Vec<u16> = format!("dl-capture-test-{}\0", uuid::Uuid::new_v4())
            .encode_utf16()
            .collect();
        let green = CreateSolidBrush(0x0033aa11);
        let wc = WNDCLASSW {
            lpfnWndProc: Some(procedure),
            lpszClassName: class.as_ptr(),
            hbrBackground: green,
            ..Default::default()
        };
        assert_ne!(RegisterClassW(&wc), 0);
        let root = CreateWindowExW(
            WS_EX_NOACTIVATE,
            class.as_ptr(),
            ptr::null(),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            80,
            80,
            240,
            160,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null(),
        );
        assert!(!root.is_null());
        let button_class: Vec<u16> = "BUTTON\0".encode_utf16().collect();
        let button = CreateWindowExW(
            0,
            button_class.as_ptr(),
            ptr::null(),
            WS_CHILD | WS_VISIBLE,
            20,
            20,
            80,
            40,
            root,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null(),
        );
        assert!(!button.is_null());
        // Transparent sibling at the same rectangle must not steal the route.
        let static_class: Vec<u16> = "STATIC\0".encode_utf16().collect();
        let transparent = CreateWindowExW(
            WS_EX_TRANSPARENT,
            static_class.as_ptr(),
            ptr::null(),
            WS_CHILD | WS_VISIBLE,
            20,
            20,
            80,
            40,
            root,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null(),
        );
        assert!(!transparent.is_null());
        UpdateWindow(root);
        let cover = CreateWindowExW(
            WS_EX_NOACTIVATE | WS_EX_TOPMOST,
            static_class.as_ptr(),
            ptr::null(),
            WS_POPUP | WS_VISIBLE,
            80,
            80,
            240,
            160,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null(),
        );
        assert!(!cover.is_null());
        UpdateWindow(cover);
        tx.send((GetCurrentThreadId(), root as usize, button as usize))
            .unwrap();
        let mut message = MSG::default();
        while GetMessageW(&mut message, ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        DestroyWindow(cover);
        DestroyWindow(root);
        UnregisterClassW(class.as_ptr(), ptr::null_mut());
        DeleteObject(green);
    });
    let (id, root, button) = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("fixture requires an unlocked interactive desktop");
    Fixture {
        thread: Some(thread),
        id,
        root,
        button,
    }
}

#[test]
#[ignore = "requires unlocked interactive Windows desktop; no IDD or Hook DLL required"]
fn handoff_accepts_application_size_constraints_but_workspace_placement_stays_exact() {
    let fixture = fixture();
    assert_ne!(
        unsafe {
            SetPropW(
                fixture.root as HWND,
                windows_sys::w!("dl-test-constrain-size"),
                1usize as _,
            )
        },
        0
    );
    let requested = daat_locus_virtual_workspace::Rect {
        left: 100,
        top: 110,
        right: 300,
        bottom: 260,
    };
    let window = Window::inspect(fixture.root).unwrap();
    let returned = window.relocate_for_handoff(requested).unwrap();
    assert_eq!(
        returned.bounds,
        daat_locus_virtual_workspace::Rect {
            left: 100,
            top: 110,
            right: 420,
            bottom: 330
        }
    );
    let error = returned.relocate(requested).unwrap_err();
    assert!(
        matches!(error, daat_locus_virtual_workspace::Error::PlacementUnsettled { requested: actual, observed }
        if actual == requested && observed == returned.bounds)
    );
    assert!(
        daat_locus_virtual_workspace::windows()
            .unwrap()
            .iter()
            .any(|w| w.handle == fixture.root)
    );
}

#[test]
#[ignore = "requires unlocked interactive Windows desktop; no IDD or Hook DLL required"]
fn workspace_placement_accepts_dpi_shrink_but_rejects_overflow() {
    use daat_locus_virtual_workspace::{Error, Rect};
    let fixture = fixture();
    assert_ne!(
        unsafe {
            SetPropW(
                fixture.root as HWND,
                windows_sys::w!("dl-test-dpi-shrink"),
                1usize as _,
            )
        },
        0
    );
    let region = Rect {
        left: 100,
        top: 110,
        right: 700,
        bottom: 560,
    };
    let window = Window::inspect(fixture.root).unwrap();
    let moved = window.relocate_in_region(region).unwrap();
    assert_eq!(
        moved.bounds,
        Rect {
            left: 100,
            top: 110,
            right: 500,
            bottom: 410
        }
    );
    let frame = moved.capture().unwrap();
    assert_eq!((frame.width, frame.height), (400, 300));
    unsafe {
        RemovePropW(fixture.root as HWND, windows_sys::w!("dl-test-dpi-shrink"));
        assert_ne!(
            SetPropW(
                fixture.root as HWND,
                windows_sys::w!("dl-test-constrain-size"),
                1usize as _
            ),
            0
        );
    }
    let small = Rect {
        left: 100,
        top: 110,
        right: 300,
        bottom: 260,
    };
    assert!(
        matches!(moved.relocate_in_region(small), Err(Error::PlacementUnsettled { observed, .. }) if observed.right == 420 && observed.bottom == 330)
    );
}

#[test]
#[ignore = "requires unlocked interactive Windows desktop; no IDD or Hook DLL required"]
fn covered_window_capture_and_child_routing() {
    unsafe {
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let fixture = fixture();
    let window = Window::inspect(fixture.root).unwrap();
    let hit = unsafe { WindowFromPoint(POINT { x: 120, y: 120 }) };
    assert_ne!(
        unsafe { GetAncestor(hit, GA_ROOT) } as usize,
        fixture.root,
        "test window must actually be covered"
    );
    for _ in 0..3 {
        // Repeated one-shot sessions must produce the target's known green pixels,
        // rather than the covering window or empty/undefined texture bytes.
        let frame = window.capture().unwrap();
        assert_eq!(frame.bounds, window.bounds);
        assert_eq!((frame.width, frame.height), (240, 160));
        let pixel = ((120 * frame.width + 180) * 4) as usize;
        assert_eq!(&frame.bgra[pixel..pixel + 4], &[0x33, 0xaa, 0x11, 255]);
        let (child, point) = window.input_at(Point { x: 40, y: 40 }).unwrap();
        assert_eq!(child, fixture.button);
        assert_eq!(point, Point { x: 20, y: 20 });
    }
    unsafe {
        EnableWindow(fixture.button as HWND, 0);
    }
    assert!(
        window.input_at(Point { x: 40, y: 40 }).is_err(),
        "disabled child must not redirect to the parent"
    );
    unsafe {
        EnableWindow(fixture.button as HWND, 1);
        EnableWindow(fixture.root as HWND, 0);
    }
    assert!(
        window.input_at(Point { x: 40, y: 40 }).is_err(),
        "modal-disabled parent must refuse input"
    );
    unsafe {
        EnableWindow(fixture.root as HWND, 1);
    }
    assert!(window.input_at(Point { x: 241, y: 20 }).is_err());
    drop(fixture);
}

#[test]
#[ignore = "requires unlocked interactive Windows desktop and stationary cursor; no IDD or Hook DLL required"]
fn one_shot_capture_preserves_desktop_state() {
    let before = DesktopState::read().unwrap();
    let fixture = fixture();
    let window = Window::inspect(fixture.root).unwrap();
    for _ in 0..3 {
        window.capture().unwrap();
        assert_eq!(DesktopState::read().unwrap(), before);
    }
    drop(fixture);
    assert_eq!(DesktopState::read().unwrap(), before);
}

#[test]
#[ignore = "requires unlocked interactive Windows desktop; no IDD or Hook DLL required"]
fn capture_survives_caller_apartment_exit_and_com_library_cleanup() {
    use windows::Win32::System::{Com::CoFreeUnusedLibrariesEx, WinRT::*};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;

    const CHILD: &str = "DAAT_LOCUS_CAPTURE_LIFETIME_CHILD";
    if std::env::var_os(CHILD).is_none() {
        // A native use-after-unload terminates the process, so exercise it in an
        // isolated child and assert its exit status as well as its pixel checks.
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "capture_survives_caller_apartment_exit_and_com_library_cleanup",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD, "1")
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(90);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success(), "capture lifetime child failed: {status}");
                return;
            }
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("capture lifetime child did not terminate");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    let fixture = fixture();
    let window = Window::inspect(fixture.root).unwrap();
    for round in 0..16 {
        let target = window.clone();
        let frame = std::thread::spawn(move || {
            // Include callers already in an STA as well as short-lived MTAs.
            unsafe {
                RoInitialize(if round % 2 == 0 {
                    RO_INIT_MULTITHREADED
                } else {
                    RO_INIT_SINGLETHREADED
                })
                .unwrap();
            }
            let frame = target.capture().unwrap();
            unsafe { RoUninitialize() };
            frame
        })
        .join()
        .unwrap();
        assert_eq!(frame.bounds, window.bounds);
        let pixel = ((120 * frame.width + 180) * 4) as usize;
        assert_eq!(&frame.bgra[pixel..pixel + 4], &[0x33, 0xaa, 0x11, 255]);
        unsafe { CoFreeUnusedLibrariesEx(0, None) };
        assert!(
            !unsafe { GetModuleHandleW(windows_sys::w!("GraphicsCapture.dll")) }.is_null(),
            "WGC callback code must remain mapped after caller teardown"
        );
    }
    // A stale/destroyed window must still fail without invalidating later captures.
    drop(fixture);
    assert!(window.capture().is_err());
    let replacement = self::fixture();
    Window::inspect(replacement.root)
        .unwrap()
        .capture()
        .unwrap();
}

#[test]
#[ignore = "requires unlocked interactive Windows desktop and stationary cursor; no IDD or Hook DLL required"]
fn minimized_window_is_discoverable_without_restoring_or_enabling_input() {
    use daat_locus_virtual_workspace::windows;
    let fixture = fixture();
    let restored = Window::inspect(fixture.root).unwrap();
    unsafe {
        ShowWindow(fixture.root as HWND, SW_SHOWMINNOACTIVE);
    }
    let minimized = Window::inspect(fixture.root).unwrap();
    assert!(minimized.minimized);
    assert!(
        restored.validate().is_err(),
        "the old geometry must be stale"
    );
    let before = DesktopState::read().unwrap();
    let mut placement = WINDOWPLACEMENT {
        length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
        ..Default::default()
    };
    assert_ne!(
        unsafe { GetWindowPlacement(fixture.root as HWND, &mut placement) },
        0
    );
    let found = windows()
        .unwrap()
        .into_iter()
        .find(|w| w.handle == fixture.root)
        .expect("discovery must include a minimized visible top-level window");
    assert_eq!(found, minimized);
    assert_eq!(found.restored_bounds().unwrap(), restored.bounds);
    assert!(!found.input_surfaces().unwrap().is_empty());
    assert!(found.validate().is_err());
    assert!(found.capture().is_err());
    assert!(found.input_at(Point { x: 10, y: 10 }).is_err());
    assert_eq!(DesktopState::read().unwrap(), before);
    assert!(Window::inspect(fixture.root).unwrap().minimized);
    let mut after = WINDOWPLACEMENT {
        length: placement.length,
        ..Default::default()
    };
    assert_ne!(
        unsafe { GetWindowPlacement(fixture.root as HWND, &mut after) },
        0
    );
    assert_eq!(after.showCmd, placement.showCmd);
    assert_eq!(
        (
            after.rcNormalPosition.left,
            after.rcNormalPosition.top,
            after.rcNormalPosition.right,
            after.rcNormalPosition.bottom
        ),
        (
            placement.rcNormalPosition.left,
            placement.rcNormalPosition.top,
            placement.rcNormalPosition.right,
            placement.rcNormalPosition.bottom
        )
    );
    unsafe {
        ShowWindow(fixture.root as HWND, SW_HIDE);
    }
    assert!(
        !windows().unwrap().iter().any(|w| w.handle == fixture.root),
        "hidden helper windows stay excluded"
    );
}
