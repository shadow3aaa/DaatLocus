#![cfg(windows)]

#[path = "../examples/support/mod.rs"]
mod support;

/// Explicit opt-in: creates only disposable windows, activates its own human window,
/// and injects genuine touch on an owned IDD display created for the test. Missing
/// prerequisites fail clearly. The signed driver must be installed; no screen need exist.
#[test]
#[ignore = "requires an interactive Windows desktop, installed signed Daat Locus driver, and stationary physical cursor"]
fn same_process_same_thread_pointer_delivery_preserves_human_input() {
    let screen = daat_locus_virtual_workspace::VirtualDisplay::create(Default::default()).unwrap();
    support::run(&screen.display().unwrap().device_name, false, true, false).unwrap();
    screen.remove().unwrap();
}

#[test]
#[ignore = "measures default activation on an owned IDD screen; requires the signed driver and interactive desktop"]
fn default_pointer_activation_on_owned_display_is_detected() {
    let screen = daat_locus_virtual_workspace::VirtualDisplay::create(Default::default()).unwrap();
    support::run(&screen.display().unwrap().device_name, true, true, false).unwrap();
    screen.remove().unwrap();
}

#[test]
#[ignore = "measures WS_EX_NOACTIVATE with default pointer activation; requires the signed driver and interactive desktop"]
fn noactivate_style_does_not_prevent_default_pointer_activation_on_owned_display() {
    let screen = daat_locus_virtual_workspace::VirtualDisplay::create(Default::default()).unwrap();
    support::run_with_options(
        &screen.display().unwrap().device_name,
        support::LabOptions {
            default_activation: true,
            automated: true,
            noactivate_window: true,
            ..Default::default()
        },
    )
    .unwrap();
    screen.remove().unwrap();
}

#[test]
#[ignore = "creates/resizes/removes an owned IDD screen; requires an installed signed Daat Locus driver"]
fn owned_display_lifecycle_preserves_existing_outputs() {
    use daat_locus_virtual_workspace::{DisplayOptions, Point, VirtualDisplay, displays};
    let before = displays().unwrap();
    let mut screen = VirtualDisplay::create(DisplayOptions {
        width: 1280,
        height: 720,
        ..Default::default()
    })
    .unwrap();
    let created = screen.display().unwrap();
    assert!(created.indirect && !created.cloned);
    assert_eq!(created.bounds.width().unwrap(), 1280);
    assert_eq!(created.bounds.height().unwrap(), 720);
    assert!(!before.iter().any(|display| display.id == created.id));
    for expected in &before {
        let actual = displays().unwrap();
        assert!(
            actual.contains(expected),
            "output changed after create: expected={expected:?}, actual={actual:?}"
        );
    }
    let origin = Point {
        x: before
            .iter()
            .map(|display| display.bounds.left)
            .min()
            .unwrap()
            - 1600,
        y: 0,
    };
    screen
        .configure(DisplayOptions {
            width: 1600,
            height: 900,
            origin: Some(origin),
            ..Default::default()
        })
        .unwrap();
    let resized = screen.display().unwrap();
    assert_eq!(resized.bounds.left, origin.x);
    assert_eq!(resized.bounds.width().unwrap(), 1600);
    assert_eq!(resized.bounds.height().unwrap(), 900);
    for expected in &before {
        let actual = displays().unwrap();
        assert!(
            actual.contains(expected),
            "output changed after configure: expected={expected:?}, actual={actual:?}"
        );
    }
    screen.remove().unwrap();
    let after = displays().unwrap();
    assert!(!after.iter().any(|display| display.id == resized.id));
    for expected in before {
        assert!(after.contains(&expected));
    }
}

#[test]
#[ignore = "requires an interactive Windows desktop and stationary physical cursor; uses disposable windows"]
fn native_pointer_metadata_and_input_preservation() {
    let display = daat_locus_virtual_workspace::displays()
        .unwrap()
        .into_iter()
        .find(|d| !d.indirect && !d.cloned)
        .expect("active physical display required");
    support::run(&display.device_name, false, true, true).unwrap();
}

#[test]
#[ignore = "requires an interactive Windows desktop; measures default activation using only disposable windows"]
fn default_pointer_activation_is_detected() {
    let display = daat_locus_virtual_workspace::displays()
        .unwrap()
        .into_iter()
        .find(|d| !d.indirect && !d.cloned)
        .expect("active physical display required");
    support::run(&display.device_name, true, true, true).unwrap();
}

#[test]
#[ignore = "requires the signed Daat Locus driver installed on an interactive Windows desktop"]
fn closing_one_workspace_preserves_another_on_the_same_adapter() {
    use daat_locus_virtual_workspace::{VirtualDisplay, displays};
    let first = VirtualDisplay::create(Default::default()).unwrap();
    let first_id = first.display().unwrap().id;
    let second = VirtualDisplay::create(Default::default()).unwrap();
    let expected = second.display().unwrap();
    assert_eq!(first_id.adapter, expected.id.adapter);
    assert_ne!(first_id.target, expected.id.target);
    first.remove().unwrap();
    assert_eq!(second.display().unwrap(), expected);
    assert!(
        !displays()
            .unwrap()
            .iter()
            .any(|display| display.id == first_id)
    );
    second.remove().unwrap();
}

#[test]
#[ignore = "terminates a disposable client to verify OS file cleanup; requires an installed signed Daat Locus driver"]
fn owned_monitor_is_removed_when_client_process_is_terminated() {
    use daat_locus_virtual_workspace::{VirtualDisplay, displays};
    use std::{
        io::{BufRead, Write},
        process::{Command, Stdio},
        time::{Duration, Instant},
    };
    const CHILD: &str = "DAAT_LOCUS_IDD_CRASH_TEST_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let screen = VirtualDisplay::create(Default::default()).unwrap();
        let id = screen.display().unwrap().id;
        println!("\nDL_IDD_MONITOR={}:{}", id.adapter, id.target);
        std::io::stdout().flush().unwrap();
        std::thread::sleep(Duration::from_secs(60));
        panic!("parent did not terminate its disposable IDD client");
    }
    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let keeper = VirtualDisplay::create(Default::default()).unwrap();
    let keeper_id = keeper.display().unwrap().id;
    // Exercise connector reuse while another lease keeps the adapter active.
    // Killing just after arrival also overlaps asynchronous swapchain setup.
    for iteration in 0..8 {
        let mut child = Child(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--ignored",
                    "--exact",
                    "owned_monitor_is_removed_when_client_process_is_terminated",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(CHILD, "1")
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        );
        let stdout = child.0.stdout.take().unwrap();
        let (send, receive) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in std::io::BufReader::new(stdout).lines().take(64) {
                let line = line.unwrap();
                if let Some(id) = line.strip_prefix("DL_IDD_MONITOR=") {
                    let _ = send.send(id.to_owned());
                    break;
                }
            }
        });
        let id = receive
            .recv_timeout(Duration::from_secs(30))
            .expect("child did not create its owned monitor");
        let (adapter, target) = id.split_once(':').unwrap();
        let adapter: u64 = adapter.parse().unwrap();
        let target: u32 = target.parse().unwrap();
        assert!(
            displays()
                .unwrap()
                .iter()
                .any(|display| display.id.adapter == adapter && display.id.target == target)
        );
        child.0.kill().unwrap();
        child.0.wait().unwrap();
        reader.join().unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if !displays()
                .unwrap()
                .iter()
                .any(|display| display.id.adapter == adapter && display.id.target == target)
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "client death leaked its IDD monitor"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(keeper.display().unwrap().id, keeper_id);
        println!(
            "terminated disposable IDD client {}/8; surviving lease remains valid",
            iteration + 1
        );
    }
    keeper.remove().unwrap();
}
