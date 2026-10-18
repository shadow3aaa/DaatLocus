mod automation;
mod capture;
mod display;
mod driver_device;
mod driver_install;
#[cfg(feature = "driver-setup")]
mod driver_trust;
mod hook;
mod hook_group;
mod input;
mod launch;
mod managed_display;
mod path_modes;
mod pointer;
mod shared_display;
mod window;

use std::{marker::PhantomData, rc::Rc};

use windows_sys::Win32::{
    Foundation::HWND,
    UI::{
        HiDpi::{
            DPI_AWARENESS_CONTEXT, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
            SetThreadDpiAwarenessContext,
        },
        WindowsAndMessaging::*,
    },
};

use crate::{ContactPhase, Display, DisplayId, Error, Point, Rect, Result};
pub use capture::Frame;
pub use display::displays;
pub use driver_install::DriverPackage;
#[cfg(feature = "driver-setup")]
pub use driver_trust::{
    install_bundled_driver, uninstall_bundled_driver, verify_bundled_driver_identity,
};
pub use hook::{HookReceipt, KeyModifiers, MouseAction};
pub use hook_group::WindowHook;
pub use input::{InputBackend, WindowInput, WindowInputReceipt};
pub use launch::{LaunchedProcess, launch_on_display};
pub use managed_display::VirtualDisplay;
pub use pointer::TouchDevice;
pub use shared_display::{DisplayRegion, SharedDisplay};
pub use window::{InputSurface, Window, windows};

fn last_error(operation: &'static str) -> Error {
    Error::Windows {
        operation,
        source: std::io::Error::last_os_error(),
    }
}

fn status_error(operation: &'static str, status: i32) -> Error {
    let status = if status as u32 & 0xffff0000 == 0x80070000 {
        status & 0xffff
    } else {
        status
    };
    Error::Windows {
        operation,
        source: std::io::Error::from_raw_os_error(status),
    }
}

/// Temporary thread-local DPI context; never changes the host application's process setting.
struct DpiContext(DPI_AWARENESS_CONTEXT);
impl DpiContext {
    fn enter() -> Result<Self> {
        let old =
            unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        if old.is_null() {
            return Err(last_error("SetThreadDpiAwarenessContext"));
        }
        Ok(Self(old))
    }
}
impl Drop for DpiContext {
    fn drop(&mut self) {
        unsafe {
            SetThreadDpiAwarenessContext(self.0);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DesktopState {
    pub foreground: usize,
    pub focus: usize,
    pub capture: usize,
    pub cursor: Point,
}

impl DesktopState {
    pub fn read() -> Result<Self> {
        let _dpi = DpiContext::enter()?;
        let foreground = unsafe { GetForegroundWindow() };
        if foreground.is_null() {
            return Err(Error::InvalidState("no interactive foreground window"));
        }
        let thread = unsafe { GetWindowThreadProcessId(foreground, std::ptr::null_mut()) };
        let mut gui = GUITHREADINFO {
            cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        if unsafe { GetGUIThreadInfo(thread, &mut gui) } == 0 {
            return Err(last_error("GetGUIThreadInfo"));
        }
        let mut cursor = Default::default();
        if unsafe { GetCursorPos(&mut cursor) } == 0 {
            return Err(last_error("GetCursorPos"));
        }
        Ok(Self {
            foreground: foreground as usize,
            focus: gui.hwndFocus as usize,
            capture: gui.hwndCapture as usize,
            cursor: Point {
                x: cursor.x,
                y: cursor.y,
            },
        })
    }
}

/// Observations immediately before/after OS acceptance; not proof of app execution or
/// absence of transient focus changes. A human may legitimately move their cursor.
#[derive(Debug)]
pub struct InputReceipt {
    pub before: DesktopState,
    pub after: DesktopState,
}
impl InputReceipt {
    pub fn focus_changed(&self) -> bool {
        self.before.foreground != self.after.foreground || self.before.focus != self.after.focus
    }
    pub fn cursor_changed(&self) -> bool {
        self.before.cursor != self.after.cursor
    }
    pub fn capture_changed(&self) -> bool {
        self.before.capture != self.after.capture
    }
}

/// One thread owns a workspace and its synthetic contact. Resources are independent of
/// the Actor/Tauri lifecycle. Dropping cancels our contact and unplugs an owned display.
/// It never closes user windows, restores focus, removes borrowed displays or uninstalls a driver.
pub struct VirtualWorkspace {
    display: Display,
    pointer: TouchDevice,
    contact_target: Option<Window>,
    interrupted: bool,
    expected_focus: Option<(usize, usize)>,
    _thread: PhantomData<Rc<()>>,
    // Declared after the input device: touch is released before the owned screen disappears.
    owned_display: Option<VirtualDisplay>,
}

impl VirtualWorkspace {
    /// Create an owned virtual output. The driver package is provisioned once through
    /// DriverPackage::install; no virtual display needs to exist beforehand.
    pub fn create(options: crate::DisplayOptions) -> Result<Self> {
        let owned = VirtualDisplay::create(options)?;
        let mut workspace = Self::attach(&owned.display()?.id)?;
        workspace.owned_display = Some(owned);
        Ok(workspace)
    }

    pub fn configure(&mut self, options: crate::DisplayOptions) -> Result<()> {
        if self.contact_target.is_some() {
            return Err(Error::InvalidState(
                "cancel active touch before changing display settings",
            ));
        }
        let owned = self.owned_display.as_mut().ok_or(Error::InvalidState(
            "cannot configure a display this workspace does not own",
        ))?;
        owned.configure(options)?;
        self.display = owned.display()?;
        self.expected_focus = None;
        self.interrupted = false;
        Ok(())
    }

    /// Explicitly release our input and wait for our owned monitor to disappear.
    pub fn close(mut self) -> Result<()> {
        self.cancel()?;
        if let Some(owned) = self.owned_display.take() {
            owned.remove()?;
        }
        Ok(())
    }

    pub fn attach(id: &DisplayId) -> Result<Self> {
        let active = displays()?;
        let display = active
            .iter()
            .find(|d| &d.id == id)
            .cloned()
            .ok_or(Error::InvalidState("workspace display is no longer active"))?;
        if !display.indirect || display.cloned {
            return Err(Error::InvalidState(
                "workspace requires an extended IDD display",
            ));
        }
        if !active.iter().any(|d| !d.indirect && !d.cloned) {
            return Err(Error::InvalidState(
                "a separate physical display must remain active",
            ));
        }
        Ok(Self {
            display,
            pointer: TouchDevice::new()?,
            contact_target: None,
            interrupted: false,
            expected_focus: None,
            _thread: PhantomData,
            owned_display: None,
        })
    }

    pub fn display(&self) -> &Display {
        &self.display
    }

    /// Hot-install the optional input DLL only for a window wholly in this workspace.
    /// Keep the returned guard alive while injecting touch; close it before user takeover.
    pub fn hook(&self, target: &Window, dll: impl AsRef<std::path::Path>) -> Result<WindowHook> {
        self.validate_display()?;
        target.validate()?;
        if !self.display.bounds.contains_rect(target.bounds) {
            return Err(Error::InvalidState(
                "hook target must fit entirely inside the workspace",
            ));
        }
        WindowHook::attach(target, dll)
    }

    /// Select the input backend before any application effect. Signature-restricted
    /// windows use accessibility patterns; failed operations are never retried.
    pub fn input(&self, target: &Window, dll: impl AsRef<std::path::Path>) -> Result<WindowInput> {
        self.validate_display()?;
        target.validate()?;
        if !self.display.bounds.contains_rect(target.bounds) {
            return Err(Error::InvalidState(
                "input target must fit entirely inside the workspace",
            ));
        }
        WindowInput::attach(target, dll)
    }

    /// Snapshot changes require explicit re-attachment, including resolution/origin changes.
    fn validate_display(&self) -> Result<()> {
        if let Some(owned) = &self.owned_display {
            owned.validate_lease()?;
        }
        let active = displays()?;
        if !active.iter().any(|d| d == &self.display) {
            return Err(Error::DisplayChanged {
                expected: Box::new(self.display.clone()),
                observed: active
                    .iter()
                    .find(|display| display.id == self.display.id)
                    .cloned()
                    .map(Box::new),
            });
        }
        if !active.iter().any(|d| !d.indirect && !d.cloned) {
            return Err(Error::InvalidState("physical display is no longer active"));
        }
        Ok(())
    }

    pub fn capture(&self) -> Result<Frame> {
        self.validate_display()?;
        let frame = capture::capture(self.display.bounds)?;
        self.validate_display()?;
        Ok(frame)
    }

    /// Placement uses physical screen coordinates and never implicitly restores a minimized
    /// or maximized application. Placement may be queued by Windows; inspect geometry again
    /// before input. The receipt describes immediate desktop observations, not completion.
    pub fn place(&mut self, target: &Window, bounds: Rect) -> Result<InputReceipt> {
        let _dpi = DpiContext::enter()?;
        self.validate_display()?;
        target.validate()?;
        if self.interrupted || self.contact_target.is_some() {
            return Err(Error::InvalidState(
                "workspace interrupted or touch contact still active",
            ));
        }
        if !self.display.bounds.contains_rect(bounds) {
            return Err(Error::InvalidState(
                "window placement is outside the workspace",
            ));
        }
        let hwnd = target.handle as HWND;
        if unsafe { IsIconic(hwnd) != 0 || IsZoomed(hwnd) != 0 } {
            return Err(Error::InvalidState("restore the window before placing it"));
        }
        let before = DesktopState::read()?;
        if let Some(expected) = self.expected_focus
            && expected != (before.foreground, before.focus)
        {
            self.interrupted = true;
            return Err(Error::InvalidState(
                "foreground or focus changed between operations; cancel and observe",
            ));
        }
        if unsafe {
            SetWindowPos(
                hwnd,
                std::ptr::null_mut(),
                bounds.left,
                bounds.top,
                bounds.width()?,
                bounds.height()?,
                SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_ASYNCWINDOWPOS,
            )
        } == 0
        {
            return Err(last_error("SetWindowPos"));
        }
        // Mark uncertain effects before observing, which itself can fail after placement.
        self.interrupted = true;
        let receipt = InputReceipt {
            before,
            after: DesktopState::read()?,
        };
        self.interrupted = receipt.focus_changed();
        self.expected_focus = Some((before.foreground, before.focus));
        Ok(receipt)
    }

    /// Coordinates are local to the IDD display. Injection is hit-tested by Windows;
    /// hwndTarget is not an injection override. Occlusion, stale geometry and contact
    /// retargeting are refused before injection. Any accepted effect remains uncertain
    /// until the caller observes the target app; it must not automatically replay it.
    pub fn touch(
        &mut self,
        target: &Window,
        phase: ContactPhase,
        local: Point,
    ) -> Result<InputReceipt> {
        let _dpi = DpiContext::enter()?;
        if self.interrupted {
            return Err(Error::InvalidState(
                "focus changed; cancel and attach again before input",
            ));
        }
        self.validate_display()?;
        target.validate()?;
        if !self.display.bounds.contains_rect(target.bounds) {
            return Err(Error::InvalidState(
                "target window must fit entirely inside the workspace",
            ));
        }
        if let Some(current) = &self.contact_target
            && current != target
        {
            return Err(Error::InvalidState(
                "touch contact cannot switch target windows",
            ));
        }
        let point = self.display.bounds.screen_point(local)?;
        window::validate_hit(target, point)?;
        let before = DesktopState::read()?;
        if let Some(expected) = self.expected_focus
            && expected != (before.foreground, before.focus)
        {
            self.interrupted = true;
            return Err(Error::InvalidState(
                "foreground or focus changed between operations; cancel and observe",
            ));
        }
        self.expected_focus = Some((before.foreground, before.focus));
        self.pointer.inject(phase, point)?;
        self.contact_target = match phase {
            ContactPhase::Up | ContactPhase::Cancel => None,
            _ => Some(target.clone()),
        };
        // Mark uncertain execution before any fallible observation after an accepted frame.
        self.interrupted = true;
        let receipt = InputReceipt {
            before,
            after: DesktopState::read()?,
        };
        self.interrupted = receipt.focus_changed();
        Ok(receipt)
    }

    /// Always permitted for our active contact, even after topology/focus/target changes.
    /// This releases input state only and never restores the user's focus or cursor.
    pub fn cancel(&mut self) -> Result<()> {
        self.pointer.cancel()?;
        self.contact_target = None;
        Ok(())
    }
}
