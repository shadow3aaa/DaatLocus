#![cfg(windows)]
use daat_locus_virtual_workspace::{DesktopState, SharedDisplay, launch_on_display, windows};
use std::{
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::HWND,
    System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
        PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject,
    },
    UI::WindowsAndMessaging::*,
};

#[test]
#[ignore = "requires signed IDD, built launch_target example, unlocked desktop and stationary cursor; run serially"]
fn initial_visible_window_uses_reserved_virtual_coordinates_without_moving_the_desktop() {
    let example = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("examples/launch_target.exe");
    assert!(example.is_file(), "build --example launch_target first");
    let service = SharedDisplay::new(Default::default()).unwrap();
    let region = service.reserve(960, 720).unwrap();
    let baseline = DesktopState::read().unwrap();
    let launched = launch_on_display(
        example.to_str().unwrap(),
        &region.display().id,
        region.bounds(),
    )
    .unwrap()
    .unwrap();
    struct Fixture(OwnedHandle);
    impl Drop for Fixture {
        fn drop(&mut self) {
            unsafe {
                TerminateProcess(self.0.as_raw_handle(), 0);
                WaitForSingleObject(self.0.as_raw_handle(), 2000);
            }
        }
    }
    let handle = unsafe {
        OpenProcess(
            PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            launched.process_id,
        )
    };
    assert!(!handle.is_null());
    let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
    let mut times = [windows_sys::Win32::Foundation::FILETIME::default(); 4];
    let [created, exited, kernel, user] = &mut times;
    assert_ne!(
        unsafe { GetProcessTimes(handle.as_raw_handle(), created, exited, kernel, user) },
        0
    );
    assert_eq!(
        (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime),
        launched.process_started
    );
    let _fixture = Fixture(handle);
    let deadline = Instant::now() + Duration::from_secs(5);
    let target = loop {
        if let Some(target) = windows().unwrap().into_iter().find(|w| {
            w.process_id == launched.process_id && w.process_started == launched.process_started
        }) {
            break target;
        }
        assert!(Instant::now() < deadline, "new fixture window unavailable");
        std::thread::sleep(Duration::from_millis(20));
    };
    let mut first = [0i32; 4];
    for (index, coordinate) in first.iter_mut().enumerate() {
        let mut value = 0;
        assert_ne!(
            unsafe {
                SendMessageTimeoutW(
                    target.handle as HWND,
                    WM_APP + 73,
                    index,
                    0,
                    SMTO_ABORTIFHUNG | SMTO_BLOCK,
                    1000,
                    &mut value,
                )
            },
            0
        );
        *coordinate = value as isize as i32;
    }
    let expected = region.bounds();
    assert_eq!(
        first,
        [expected.left, expected.top, expected.right, expected.bottom],
        "a later move cannot satisfy first-show placement"
    );
    assert_eq!(target.bounds, expected);
    assert_eq!(
        DesktopState::read().unwrap(),
        baseline,
        "startup changed the physical cursor/focus/capture"
    );
}

#[test]
#[ignore = "requires installed Calculator and signed IDD, no existing Calculator window, unlocked desktop; run serially"]
fn calculator_shell_activation_respects_the_virtual_monitor_hint() {
    const APP: &str = "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App";
    let before = windows().unwrap();
    assert!(
        !before
            .iter()
            .any(|w| w.has_application_id(APP).unwrap_or(false)),
        "close existing Calculator windows before this opt-in check"
    );
    unsafe extern "system" fn collect(hwnd: HWND, data: isize) -> i32 {
        unsafe { &mut *(data as *mut std::collections::BTreeSet<usize>) }.insert(hwnd as usize);
        1
    }
    let mut original = std::collections::BTreeSet::<usize>::new();
    assert_ne!(
        unsafe { EnumWindows(Some(collect), &mut original as *mut _ as isize) },
        0
    );
    let service = SharedDisplay::new(Default::default()).unwrap();
    let region = service.reserve(960, 720).unwrap();
    let baseline = DesktopState::read().unwrap();
    let receipt = launch_on_display(
        &format!("shell:AppsFolder\\{APP}"),
        &region.display().id,
        region.bounds(),
    )
    .unwrap();
    eprintln!("Shell launch receipt: {receipt:?}");
    let deadline = Instant::now() + Duration::from_secs(8);
    let target =
        loop {
            if let Some(window) = windows().unwrap().into_iter().find(|w| {
                !original.contains(&w.handle) && w.has_application_id(APP).unwrap_or(false)
            }) {
                break window;
            }
            assert!(
                Instant::now() < deadline,
                "no new Calculator window; existing/hidden windows are never acquired"
            );
            std::thread::sleep(Duration::from_millis(50));
        };
    struct Calculator(daat_locus_virtual_workspace::Window);
    impl Drop for Calculator {
        fn drop(&mut self) {
            if let Ok(now) = daat_locus_virtual_workspace::Window::inspect(self.0.handle)
                && now.process_id == self.0.process_id
                && now.process_started == self.0.process_started
                && now.thread_id == self.0.thread_id
                && now.has_application_id(APP).unwrap_or(false)
            {
                unsafe {
                    PostMessageW(now.handle as HWND, WM_CLOSE, 0, 0);
                }
            }
        }
    }
    let target = Calculator(target);
    eprintln!(
        "Calculator startup bounds: {:?}; virtual bounds: {:?}; desktop before: {:?}; desktop after: {:?}",
        target.0.bounds,
        region.display().bounds,
        baseline,
        DesktopState::read().unwrap()
    );
    assert!(
        region.display().bounds.contains_rect(target.0.bounds),
        "Calculator ignored the Shell virtual-monitor hint"
    );
}
