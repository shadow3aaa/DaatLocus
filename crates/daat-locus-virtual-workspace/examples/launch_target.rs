//! Disposable native target for first-show startup placement tests.
#[cfg(windows)]
fn main() {
    use std::{cell::Cell, ptr};
    use windows_sys::Win32::{
        Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM},
        System::LibraryLoader::GetModuleHandleW,
        UI::{HiDpi::*, WindowsAndMessaging::*},
    };
    thread_local! {
        static FIRST: Cell<Option<RECT>> = const { Cell::new(None) };
    }
    unsafe extern "system" fn procedure(hwnd: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
        match message {
            WM_SHOWWINDOW if w != 0 => {
                if FIRST.get().is_none() {
                    let mut rect = RECT::default();
                    if unsafe { GetWindowRect(hwnd, &mut rect) } != 0 {
                        FIRST.set(Some(rect));
                    }
                }
            }
            message if message == WM_APP + 73 => {
                return FIRST.get().map_or(isize::MIN, |r| match w {
                    0 => r.left as isize,
                    1 => r.top as isize,
                    2 => r.right as isize,
                    3 => r.bottom as isize,
                    _ => isize::MIN,
                });
            }
            message if message == WM_APP + 74 => {
                return std::env::var_os("PATH").is_some() as isize;
            }
            message if message == WM_APP + 75 => {
                return std::env::var_os("SystemRoot").is_some() as isize;
            }
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
    unsafe {
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let module = GetModuleHandleW(ptr::null());
        let class = windows_sys::core::w!("DaatLocusLaunchTarget");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(procedure),
            hInstance: module,
            lpszClassName: class,
            hCursor: LoadCursorW(ptr::null_mut(), IDC_ARROW),
            ..Default::default()
        };
        assert_ne!(RegisterClassW(&wc), 0);
        let ignore_placement = std::env::current_exe()
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .contains("ignore-position");
        let initial = if ignore_placement {
            [100, 100, 640, 480]
        } else {
            [CW_USEDEFAULT; 4]
        };
        let hwnd = CreateWindowExW(
            0,
            class,
            windows_sys::core::w!("Launch placement fixture"),
            WS_OVERLAPPEDWINDOW,
            initial[0],
            initial[1],
            initial[2],
            initial[3],
            ptr::null_mut(),
            ptr::null_mut(),
            module,
            ptr::null(),
        );
        assert!(!hwnd.is_null());
        ShowWindow(
            hwnd,
            if ignore_placement {
                SW_SHOWNOACTIVATE
            } else {
                SW_SHOWDEFAULT
            },
        );
        let mut message = MSG::default();
        loop {
            let result = GetMessageW(&mut message, ptr::null_mut(), 0, 0);
            assert_ne!(result, -1);
            if result == 0 {
                break;
            }
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("requires Windows");
}
