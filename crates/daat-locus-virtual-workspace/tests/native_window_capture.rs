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
            lpfnWndProc: Some(DefWindowProcW),
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
