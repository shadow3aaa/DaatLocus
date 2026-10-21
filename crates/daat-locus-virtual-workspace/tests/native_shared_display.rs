#![cfg(windows)]
use daat_locus_virtual_workspace::{DesktopState, SharedDisplay, displays};

#[test]
#[ignore = "requires installed signed IDD and an unlocked desktop; run native desktop tests serially"]
fn workspace_adapter_identity_preserves_user_outputs_including_indirect_displays() {
    let before = displays().unwrap();
    let user = before
        .iter()
        .filter(|display| !display.workspace)
        .cloned()
        .collect::<Vec<_>>();
    assert!(!user.is_empty(), "an active user output is required");
    let service = SharedDisplay::new(Default::default()).unwrap();
    let owned = service.initialize().unwrap();
    assert!(
        owned.workspace,
        "our output must be identified by adapter hardware identity"
    );
    let workspace = daat_locus_virtual_workspace::VirtualWorkspace::attach(&owned.id).unwrap();
    assert_eq!(workspace.capture().unwrap().bounds, owned.bounds);
    let during = displays().unwrap();
    assert_eq!(
        during
            .iter()
            .filter(|display| !display.workspace)
            .cloned()
            .collect::<Vec<_>>(),
        user
    );
    assert!(
        during
            .iter()
            .any(|display| display.id == owned.id && display.workspace)
    );
    drop(workspace);
    drop(service);
    assert_eq!(
        displays().unwrap(),
        before,
        "workspace cleanup must retain every existing output"
    );
}

#[test]
#[ignore = "requires installed signed IDD, unlocked desktop and stationary cursor; run native desktop tests serially"]
fn settings_resize_reuses_the_output_and_disabling_fences_lazy_recreation() {
    let before = displays().unwrap();
    let service = SharedDisplay::new(Default::default()).unwrap();
    let original = service.initialize().unwrap();
    let region = service.reserve(600, 400).unwrap();
    let smaller = daat_locus_virtual_workspace::DisplayOptions {
        width: 1600,
        height: 900,
        ..Default::default()
    };
    assert!(service.configure(true, smaller).is_err());
    region.validate().unwrap();
    assert_eq!(service.inspect().unwrap().unwrap(), original);
    drop(region);
    let resized = service.configure(true, smaller).unwrap().unwrap();
    assert_eq!(
        resized.id, original.id,
        "resize must retain the same monitor"
    );
    assert_eq!(
        (
            resized.bounds.width().unwrap(),
            resized.bounds.height().unwrap()
        ),
        (1600, 900)
    );
    assert_eq!(
        (resized.bounds.left, resized.bounds.top),
        (original.bounds.left, original.bounds.top)
    );
    let topology = displays().unwrap();
    let input = DesktopState::read().unwrap();
    let preview = service.capture().unwrap().unwrap();
    assert_eq!(preview.bounds, resized.bounds);
    assert_eq!((preview.width, preview.height), (1600, 900));
    assert_eq!(preview.bgra.len(), 1600 * 900 * 4);
    assert_eq!(displays().unwrap(), topology);
    assert_eq!(DesktopState::read().unwrap(), input);
    assert_eq!(service.configure(true, smaller).unwrap().unwrap(), resized);
    assert_eq!(displays().unwrap(), topology);
    assert_eq!(DesktopState::read().unwrap(), input);
    assert!(service.configure(false, smaller).unwrap().is_none());
    assert!(service.inspect().unwrap().is_none());
    assert!(service.capture().unwrap().is_none());
    assert!(service.initialize().is_err());
    assert!(service.reserve(600, 400).is_err());
    assert_eq!(displays().unwrap(), before);
    let enabled = service.configure(true, smaller).unwrap().unwrap();
    assert!(
        displays()
            .unwrap()
            .iter()
            .any(|display| display.id == enabled.id)
    );
    drop(service);
    assert_eq!(displays().unwrap(), before);
}

#[test]
#[ignore = "requires installed signed IDD, unlocked desktop and stationary cursor; run native desktop tests serially"]
fn idle_and_concurrent_clients_keep_one_screen_until_the_service_and_regions_are_dropped() {
    let service = SharedDisplay::global().unwrap();
    let screen = service.initialize().unwrap();
    assert!(
        displays().unwrap().contains(&screen),
        "startup initialization must create a monitor before any window reservation"
    );
    let initialized = displays().unwrap();
    assert_eq!(service.initialize().unwrap(), screen);
    assert_eq!(displays().unwrap(), initialized);
    let first = service.reserve(600, 400).unwrap();
    assert_eq!(first.display(), &screen);
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
    // Full-size leases stack on the same output; capacity never forces another
    // monitor, shrinks an existing lease or refuses a new window.
    let stacked: Vec<_> = (0..20)
        .map(|_| service.reserve(i32::MAX, i32::MAX).unwrap())
        .collect();
    for region in &stacked {
        region.validate().unwrap();
        assert_eq!(region.display(), &screen);
        assert_eq!(region.bounds(), bounds);
    }
    drop(stacked);
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
