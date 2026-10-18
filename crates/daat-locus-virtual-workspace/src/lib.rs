//! Device-local Windows Virtual Workspace infrastructure, independent of Apps and GUI clients.
//!
//! The companion IDD/IddCx driver is built from `driver/`. Provision its signed package
//! through `DriverPackage`, then `VirtualWorkspace::create` owns a monitor through a device
//! file handle; no virtual monitor needs to exist beforehand. DisplayConfig manages resolution
//! and placement. The library also captures BGRA and injects genuine `WM_POINTER*` touch.
//! Optional thread-scoped DLL hooks veto target activation and dispatch text/key messages.
//! No fabricated pointer messages, mouse SendInput, or focus restoration.
//! Synthetic touch does **not** supply an independent keyboard or guarantee nonactivation:
//! the receiving application's `WM_POINTERACTIVATE` behavior still applies.
//!
//! Driver installation establishes the persistent adapter and requires administrator access.
//! Workspace creation uses device IOCTLs without a pipe server or elevation. Owned outputs
//! are unplugged when their last device handle closes. See CONTRIBUTING for diagnostics.

use thiserror::Error;

#[cfg(any(windows, test))]
#[allow(dead_code)]
mod hook_protocol;

#[cfg(test)]
#[allow(dead_code)]
mod driver_leases;
#[allow(dead_code)]
mod driver_protocol;

#[derive(Debug, Error)]
pub enum Error {
    #[error("Windows Virtual Workspace requires Windows 10 1903 or later")]
    UnsupportedPlatform,
    #[error("{operation}: {source}")]
    Windows {
        operation: &'static str,
        source: std::io::Error,
    },
    #[error("{0}")]
    InvalidState(&'static str),
    #[error("workspace display changed: expected {expected:?}, observed {observed:?}")]
    DisplayChanged {
        expected: Box<Display>,
        observed: Option<Box<Display>>,
    },
    #[error("{0} timed out")]
    Timeout(&'static str),
    #[error("input surface {class} (process {process_id}, thread {thread_id}): {source}")]
    InputSurface {
        process_id: u32,
        thread_id: u32,
        class: String,
        source: Box<Error>,
    },
    #[error("target requires Microsoft/Store-signed DLLs; workspace Hook input is unsupported")]
    HookSignaturePolicy,
    #[error("{0}")]
    UnsupportedInput(&'static str),
    #[cfg(windows)]
    #[error("{operation}: {source}")]
    Automation {
        operation: &'static str,
        source: ::windows::core::Error,
    },
    #[error(
        "virtual display device failed to start (PnP problem {0}); install the signed Daat Locus display driver"
    )]
    DeviceProblem(u32),
}

/// Modes advertised by the bundled IDD driver, all at 60 Hz.
pub const DISPLAY_MODES: &[(u32, u32)] = &[
    (1280, 720),
    (1600, 900),
    (1920, 1080),
    (2560, 1440),
    (3840, 2160),
];

#[derive(Debug, Clone, Copy)]
pub struct DisplayOptions {
    pub width: u32,
    pub height: u32,
    /// None chooses an unoccupied position adjacent to a physical display,
    /// preferring its right edge. Explicit coordinates are validated by Windows.
    pub origin: Option<Point>,
    /// Applies to each bounded PnP/topology transition.
    pub timeout: std::time::Duration,
}

impl Default for DisplayOptions {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            origin: None,
            timeout: std::time::Duration::from_secs(20),
        }
    }
}

impl DisplayOptions {
    pub fn validate(self) -> Result<()> {
        if !DISPLAY_MODES.contains(&(self.width, self.height)) {
            return Err(Error::InvalidState(
                "resolution is not advertised by the Daat Locus IDD driver",
            ));
        }
        if self.timeout.is_zero() || self.timeout > std::time::Duration::from_secs(60) {
            return Err(Error::InvalidState(
                "display transition timeout must be greater than zero and at most 60 seconds",
            ));
        }
        if let Some(origin) = self.origin {
            origin
                .x
                .checked_add(self.width as i32)
                .ok_or(Error::InvalidState("display coordinate overflow"))?;
            origin
                .y
                .checked_add(self.height as i32)
                .ok_or(Error::InvalidState("display coordinate overflow"))?;
        }
        Ok(())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Physical pixels in the Windows virtual-screen coordinate system (possibly negative).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

/// A half-open physical-pixel rectangle: left/top inclusive, right/bottom exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub fn width(self) -> Result<i32> {
        self.right
            .checked_sub(self.left)
            .filter(|v| *v > 0)
            .ok_or(Error::InvalidState("invalid rectangle width"))
    }

    pub fn height(self) -> Result<i32> {
        self.bottom
            .checked_sub(self.top)
            .filter(|v| *v > 0)
            .ok_or(Error::InvalidState("invalid rectangle height"))
    }

    pub fn contains(self, point: Point) -> bool {
        point.x >= self.left && point.x < self.right && point.y >= self.top && point.y < self.bottom
    }

    pub fn contains_rect(self, other: Self) -> bool {
        other.width().is_ok()
            && other.height().is_ok()
            && other.left >= self.left
            && other.top >= self.top
            && other.right <= self.right
            && other.bottom <= self.bottom
    }

    /// Convert display-local coordinates, rejecting overflow and out-of-display points.
    pub fn screen_point(self, local: Point) -> Result<Point> {
        let point = Point {
            x: self
                .left
                .checked_add(local.x)
                .ok_or(Error::InvalidState("coordinate overflow"))?,
            y: self
                .top
                .checked_add(local.y)
                .ok_or(Error::InvalidState("coordinate overflow"))?,
        };
        if !self.contains(point) {
            return Err(Error::InvalidState(
                "point is outside the workspace display",
            ));
        }
        Ok(point)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContactPhase {
    Down,
    Move,
    Up,
    Cancel,
}

/// One contact per synthetic device. State changes only after Windows accepts a frame.
#[derive(Debug, Default)]
#[cfg(any(windows, test))]
pub(crate) struct ContactState {
    pub point: Option<Point>,
}

#[cfg(any(windows, test))]
impl ContactState {
    pub fn validate(&self, phase: ContactPhase, point: Point) -> Result<()> {
        if matches!(phase, ContactPhase::Up | ContactPhase::Cancel) && self.point != Some(point) {
            return Err(Error::InvalidState(
                "release must use the last accepted touch location",
            ));
        }
        match (self.point.is_some(), phase) {
            (false, ContactPhase::Down)
            | (true, ContactPhase::Move | ContactPhase::Up | ContactPhase::Cancel) => Ok(()),
            _ => Err(Error::InvalidState(
                "invalid touch lifecycle; down must precede move/up/cancel",
            )),
        }
    }

    pub fn accepted(&mut self, phase: ContactPhase, point: Point) {
        self.point = match phase {
            ContactPhase::Up | ContactPhase::Cancel => None,
            _ => Some(point),
        };
    }
}

/// Identity is valid for the current display topology, not across driver reinstalls/reboots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayId {
    pub adapter: u64,
    pub target: u32,
    pub monitor_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Display {
    pub id: DisplayId,
    pub name: String,
    pub device_name: String,
    pub bounds: Rect,
    pub rotation: i32,
    /// Raw DisplayConfig output technology for diagnostics.
    pub output_technology: i32,
    pub indirect: bool,
    /// A mirrored source shares pixels with another target and cannot be a workspace.
    pub cloned: bool,
}

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;

#[cfg(not(windows))]
pub fn displays() -> Result<Vec<Display>> {
    Err(Error::UnsupportedPlatform)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_timeout_cancels_pending_effect_and_duplicate_wake_cannot_replay() {
        use crate::hook_protocol::*;
        use std::{cell::UnsafeCell, sync::atomic::AtomicU32};
        let shared = || Shared {
            abi: ABI,
            size: std::mem::size_of::<Shared>() as u32,
            owner_pid: 0,
            target_pid: 0,
            owner_started: 0,
            target_started: 0,
            root: 0,
            transport: 0,
            frame_pid: 0,
            frame_thread: 0,
            frame_started: 0,
            token: 1,
            enabled: AtomicU32::new(1),
            state: AtomicU32::new(PENDING),
            error: AtomicU32::new(0),
            completed: AtomicU32::new(0),
            activation_vetoes: AtomicU32::new(0),
            focus_vetoes: AtomicU32::new(0),
            command: UnsafeCell::new(Command::default()),
        };
        let queue = shared();
        assert!(queue.cancel_pending());
        assert!(
            !queue.claim(),
            "a delayed wake must not execute a cancelled effect"
        );
        let queue = shared();
        assert!(queue.claim());
        assert!(
            !queue.claim(),
            "duplicate wake must not replay a started effect"
        );
        assert!(
            !queue.cancel_pending(),
            "started work has an uncertain outcome on timeout"
        );
    }

    #[test]
    fn negative_origin_coordinates_and_half_open_edges() {
        let bounds = Rect {
            left: -1920,
            top: -100,
            right: 0,
            bottom: 980,
        };
        assert_eq!(
            bounds.screen_point(Point { x: 50, y: 100 }).unwrap(),
            Point { x: -1870, y: 0 }
        );
        for point in [
            Point { x: -1, y: 0 },
            Point { x: 1920, y: 0 },
            Point { x: 0, y: 1080 },
        ] {
            assert!(bounds.screen_point(point).is_err());
        }
        assert!(!bounds.contains_rect(Rect {
            left: -10,
            top: 0,
            right: 1,
            bottom: 20
        }));
        assert!(!bounds.contains_rect(Rect {
            left: -10,
            top: 0,
            right: -10,
            bottom: 20
        }));
        assert!(
            Rect {
                left: i32::MAX,
                top: 0,
                right: i32::MAX,
                bottom: 1
            }
            .screen_point(Point { x: 1, y: 0 })
            .is_err()
        );
    }

    #[test]
    fn invalid_frames_do_not_change_contact_and_cancel_allows_a_new_contact() {
        let mut state = ContactState::default();
        let point = Point { x: 12, y: 34 };
        assert!(state.validate(ContactPhase::Up, point).is_err());
        state.validate(ContactPhase::Down, point).unwrap();
        // Validation alone must not advance the state when the OS rejects injection.
        assert!(state.point.is_none());
        state.accepted(ContactPhase::Down, point);
        assert!(state.validate(ContactPhase::Down, point).is_err());
        state.validate(ContactPhase::Move, point).unwrap();
        assert!(
            state
                .validate(ContactPhase::Up, Point { x: 13, y: 34 })
                .is_err()
        );
        state.validate(ContactPhase::Cancel, point).unwrap();
        state.accepted(ContactPhase::Cancel, point);
        state.validate(ContactPhase::Down, point).unwrap();
    }
}
