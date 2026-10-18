#![cfg(windows)]
use daat_locus_virtual_workspace::{DesktopState, SharedDisplay, displays};

#[test]
#[ignore = "requires installed signed IDD, unlocked desktop and stationary cursor; run native desktop tests serially"]
fn idle_and_concurrent_clients_keep_one_screen_until_the_service_and_regions_are_dropped() {
    let service = SharedDisplay::global().unwrap();
    let first = service.reserve(600, 400).unwrap();
    let screen = first.display().clone();
    let area = first.bounds();
    std::thread::sleep(std::time::Duration::from_millis(500));
    let baseline = DesktopState::read().unwrap();
    let topology = displays().unwrap();
    // Dropping all window leases leaves the output present and its modes untouched.
    drop(first);
    let full = service.reserve(i32::MAX, i32::MAX).unwrap();
    use windows_sys::Win32::{
        Foundation::RECT,
        Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONULL, MONITORINFO, MonitorFromRect},
    };
    let bounds = full.bounds();
    let native = RECT {
        left: bounds.left,
        top: bounds.top,
        right: bounds.right,
        bottom: bounds.bottom,
    };
    let monitor = unsafe { MonitorFromRect(&native, MONITOR_DEFAULTTONULL) };
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    assert_ne!(unsafe { GetMonitorInfoW(monitor, &mut info) }, 0);
    assert!(
        bounds.left >= info.rcWork.left
            && bounds.top >= info.rcWork.top
            && bounds.right <= info.rcWork.right
            && bounds.bottom <= info.rcWork.bottom,
        "large windows must not extend underneath the taskbar"
    );
    drop(full);
    let old = service.reserve(600, 400).unwrap();
    let moved = old
        .replacement(
            Some((600, 400)),
            Some(daat_locus_virtual_workspace::Point { x: 800, y: 20 }),
        )
        .unwrap();
    let spare = service.reserve(600, 400).unwrap();
    assert_ne!(
        spare.bounds(),
        old.bounds(),
        "an unresolved move freed its old position"
    );
    assert_ne!(
        spare.bounds(),
        moved.bounds(),
        "an unresolved move freed its destination"
    );
    assert_eq!(moved.display(), &screen);
    old.validate().unwrap();
    moved.validate().unwrap();
    drop(old);
    let reused = service.reserve(600, 400).unwrap();
    assert_eq!(
        reused.bounds(),
        area,
        "confirmed move did not free its old position"
    );
    drop(reused);
    drop(spare);
    drop(moved);
    let client = SharedDisplay::global().unwrap();
    let other = std::thread::spawn(move || client.reserve(600, 400).unwrap())
        .join()
        .unwrap();
    assert_eq!(other.display(), &screen);
    assert_eq!(other.bounds(), area);
    let concurrent = service.reserve(600, 400).unwrap();
    assert_eq!(concurrent.display(), &screen);
    let a = other.bounds();
    let b = concurrent.bounds();
    assert!(a.right <= b.left || b.right <= a.left || a.bottom <= b.top || b.bottom <= a.top);
    for _ in 0..4 {
        let region = service.reserve(400, 200).unwrap();
        region.validate().unwrap();
        assert_eq!(region.display(), &screen);
    }
    assert_eq!(displays().unwrap(), topology);
    assert_eq!(
        DesktopState::read().unwrap(),
        baseline,
        "warm leases changed cursor/focus/capture or topology"
    );
    drop(service);
    drop(concurrent);
    other.validate().unwrap();
    assert!(
        displays().unwrap().contains(&screen),
        "a pending window lease must retain the service"
    );
    drop(other);
    assert!(
        !displays().unwrap().iter().any(|d| d.id == screen.id),
        "last service owner did not remove its monitor"
    );
}
