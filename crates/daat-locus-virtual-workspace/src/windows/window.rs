use super::{DpiContext, last_error};
use crate::{Error, Point, Rect, Result};
use windows_sys::Win32::{
    Foundation::{CloseHandle, FILETIME, HWND, LPARAM, POINT, RECT},
    Graphics::Gdi::ClientToScreen,
    System::Threading::{GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
    UI::{Input::KeyboardAndMouse::IsWindowEnabled, WindowsAndMessaging::*},
};

/// HWND plus process creation time prevents accidentally reusing a recycled handle.
/// Geometry is part of the observation: inspect again after any placement or app layout change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    pub handle: usize,
    pub process_id: u32,
    pub thread_id: u32,
    pub process_started: u64,
    pub bounds: Rect,
    pub client_bounds: Rect,
}

/// A native input surface inside an observed window, possibly hosted by another
/// process/thread. Its process lifetime and class are independent of the frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputSurface {
    pub handle: usize,
    pub process_id: u32,
    pub thread_id: u32,
    pub process_started: u64,
    pub class: String,
}
impl InputSurface {
    pub fn inspect(frame: &Window, handle: usize) -> Result<Self> {
        frame.validate()?;
        if !frame.owns_control(handle) {
            return Err(Error::InvalidState(
                "input surface is no longer inside the controlled window",
            ));
        }
        let hwnd = handle as HWND;
        let mut process_id = 0;
        let thread_id = unsafe { GetWindowThreadProcessId(hwnd, &mut process_id) };
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process_id) };
        if process.is_null() {
            return Err(last_error("OpenProcess(input surface)"));
        }
        let started = super::super::hook_protocol::native::process_started(process);
        unsafe {
            CloseHandle(process);
        }
        let process_started =
            started.ok_or_else(|| last_error("GetProcessTimes(input surface)"))?;
        let mut class = [0u16; 256];
        let length = unsafe { GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32) };
        if thread_id == 0 || length == 0 {
            return Err(last_error("inspect input surface"));
        }
        let mut current_pid = 0;
        if unsafe { GetWindowThreadProcessId(hwnd, &mut current_pid) } != thread_id
            || current_pid != process_id
            || !frame.owns_control(handle)
        {
            return Err(Error::InvalidState(
                "input surface changed during inspection",
            ));
        }
        frame.validate()?;
        Ok(Self {
            handle,
            process_id,
            thread_id,
            process_started,
            class: String::from_utf16_lossy(&class[..length as usize]),
        })
    }
    pub fn validate(&self, frame: &Window) -> Result<()> {
        if Self::inspect(frame, self.handle)? != *self {
            return Err(Error::InvalidState("input surface identity changed"));
        }
        Ok(())
    }
}

fn rectangle(rect: RECT) -> Rect {
    Rect {
        left: rect.left,
        top: rect.top,
        right: rect.right,
        bottom: rect.bottom,
    }
}

impl Window {
    /// Match the registered application of a real root/hosted content process.
    /// Shell brokers can return no target process handle; PID proximity or a
    /// matching window title is not enough to attribute an activation.
    pub fn has_application_id(&self, application: &str) -> Result<bool> {
        use windows_sys::Win32::{
            Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS},
            Storage::Packaging::Appx::GetApplicationUserModelId,
        };
        let mut seen = std::collections::BTreeSet::new();
        for surface in self.input_surfaces()? {
            if !seen.insert(surface.process_id) {
                continue;
            }
            let process =
                unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, surface.process_id) };
            if process.is_null() {
                return Err(last_error("inspect application identity"));
            }
            let mut length = 0;
            let status =
                unsafe { GetApplicationUserModelId(process, &mut length, std::ptr::null_mut()) };
            let matches = if status == ERROR_INSUFFICIENT_BUFFER && (1..=2048).contains(&length) {
                let mut text = vec![0u16; length as usize];
                let status =
                    unsafe { GetApplicationUserModelId(process, &mut length, text.as_mut_ptr()) };
                status == ERROR_SUCCESS
                    && length > 0
                    && String::from_utf16_lossy(&text[..length as usize - 1])
                        .eq_ignore_ascii_case(application)
            } else {
                false
            };
            unsafe {
                CloseHandle(process);
            }
            if matches {
                surface.validate(self)?;
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Screen capture must not accidentally include another window rendered
    /// above this complete window. Check immediately before and after capture;
    /// this is a sampled check, not an atomic compositor capture primitive.
    pub fn validate_unoccluded(&self) -> Result<()> {
        use windows_sys::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
        let _dpi = DpiContext::enter()?;
        self.validate()?;
        let mut above = unsafe { GetWindow(self.handle as HWND, GW_HWNDPREV) };
        for _ in 0..4096 {
            if above.is_null() {
                return self.validate();
            }
            if unsafe { IsWindowVisible(above) != 0 && IsIconic(above) == 0 } {
                let mut bounds = RECT::default();
                if unsafe { GetWindowRect(above, &mut bounds) } == 0 {
                    return Err(last_error("inspect capture occlusion"));
                }
                let client = self.bounds;
                if bounds.left < client.right
                    && client.left < bounds.right
                    && bounds.top < client.bottom
                    && client.top < bounds.bottom
                {
                    let mut cloaked = 0u32;
                    let status = unsafe {
                        DwmGetWindowAttribute(
                            above,
                            DWMWA_CLOAKED as u32,
                            (&mut cloaked as *mut u32).cast(),
                            std::mem::size_of::<u32>() as u32,
                        )
                    };
                    if status < 0 || cloaked == 0 {
                        return Err(Error::InvalidState(
                            "window image is obscured; move or release the overlapping window before observing",
                        ));
                    }
                }
            }
            above = unsafe { GetWindow(above, GW_HWNDPREV) };
        }
        Err(Error::InvalidState(
            "window stacking changed during capture validation",
        ))
    }

    pub fn owns_control(&self, handle: usize) -> bool {
        if unsafe { IsWindow(handle as HWND) } == 0 {
            return false;
        }
        let mut root = unsafe { GetAncestor(handle as HWND, GA_ROOT) };
        for _ in 0..64 {
            if root as usize == self.handle {
                return true;
            }
            root = unsafe { GetWindow(root, GW_OWNER) };
            if root.is_null() {
                break;
            }
        }
        false
    }

    /// Enumerate real descendant GUI threads. This does not infer membership from PID.
    pub fn input_surfaces(&self) -> Result<Vec<InputSurface>> {
        self.validate()?;
        unsafe extern "system" fn collect(hwnd: HWND, data: LPARAM) -> i32 {
            unsafe { &mut *(data as *mut Vec<usize>) }.push(hwnd as usize);
            1
        }
        let mut handles = vec![self.handle];
        unsafe {
            EnumChildWindows(
                self.handle as HWND,
                Some(collect),
                &mut handles as *mut _ as LPARAM,
            );
        }
        let mut surfaces = Vec::<InputSurface>::new();
        for handle in handles {
            if unsafe { IsWindowVisible(handle as HWND) } == 0 {
                continue;
            }
            let surface = InputSurface::inspect(self, handle)?;
            if !surfaces.iter().any(|s| {
                s.process_id == surface.process_id
                    && s.thread_id == surface.thread_id
                    && s.process_started == surface.process_started
            }) {
                surfaces.push(surface);
            }
        }
        self.validate()?;
        Ok(surfaces)
    }
    /// Nonactivating placement outside or inside a workspace. Return a fresh observation;
    /// moving never authorizes input to the new location by itself.
    pub fn relocate(&self, bounds: Rect) -> Result<Self> {
        let _dpi = DpiContext::enter()?;
        // Display arrival can trigger a per-monitor DPI relayout after selection.
        // Placement targets identity and explicit bounds, not old image pixels;
        // only input requires the full observed geometry to remain unchanged.
        let current = Self::inspect(self.handle)?;
        if current.process_id != self.process_id
            || current.process_started != self.process_started
            || current.thread_id != self.thread_id
        {
            return Err(Error::InvalidState(
                "window identity changed before placement",
            ));
        }
        if unsafe { IsZoomed(self.handle as HWND) } != 0 {
            return Err(Error::InvalidState("restore the window before moving it"));
        }
        if unsafe {
            SetWindowPos(
                self.handle as HWND,
                std::ptr::null_mut(),
                bounds.left,
                bounds.top,
                bounds.width()?,
                bounds.height()?,
                SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_ASYNCWINDOWPOS,
            )
        } == 0
        {
            return Err(last_error("SetWindowPos(relocate)"));
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut settled = None;
        let mut previous = None;
        loop {
            let observed = Self::inspect(self.handle)?;
            if observed.process_id != self.process_id
                || observed.process_started != self.process_started
                || observed.thread_id != self.thread_id
            {
                return Err(Error::InvalidState(
                    "window identity changed during placement",
                ));
            }
            if previous.as_ref() != Some(&observed) {
                settled = None;
            }
            previous = Some(observed.clone());
            if observed.bounds == bounds {
                let since = settled.get_or_insert_with(std::time::Instant::now);
                // SetWindowPos can be followed by shell/DPI relayout. Bind the
                // result only after a short stable observation, never reissue
                // the placement to fight the application's geometry decisions.
                if since.elapsed() >= std::time::Duration::from_millis(100) {
                    return Ok(observed);
                }
            } else {
                settled = None;
            }
            if std::time::Instant::now() >= deadline {
                return Err(Error::Timeout("window placement; observe before retrying"));
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// Resolve a physical client-image point to the actual child HWND and native
    /// client coordinates. Never redirects to an occluding or sibling window.
    pub fn input_at(&self, local: Point) -> Result<(usize, Point)> {
        self.validate()?;
        let screen = self.client_bounds.screen_point(local)?;
        validate_hit(self, screen)?;
        let hit = unsafe {
            WindowFromPoint(POINT {
                x: screen.x,
                y: screen.y,
            })
        };
        Ok((hit as usize, self.control_point(hit as usize, local)?))
    }

    pub fn control_point(&self, control: usize, local: Point) -> Result<Point> {
        use windows_sys::Win32::UI::HiDpi::{
            GetWindowDpiAwarenessContext, PhysicalToLogicalPointForPerMonitorDPI,
            SetThreadDpiAwarenessContext,
        };
        self.validate()?;
        let hwnd = control as HWND;
        InputSurface::inspect(self, control)?;
        let mut point = POINT {
            x: self
                .client_bounds
                .left
                .checked_add(local.x)
                .ok_or(Error::InvalidState("coordinate overflow"))?,
            y: self
                .client_bounds
                .top
                .checked_add(local.y)
                .ok_or(Error::InvalidState("coordinate overflow"))?,
        };
        if unsafe { PhysicalToLogicalPointForPerMonitorDPI(hwnd, &mut point) } == 0 {
            return Err(last_error("PhysicalToLogicalPointForPerMonitorDPI"));
        }
        let old = unsafe { SetThreadDpiAwarenessContext(GetWindowDpiAwarenessContext(hwnd)) };
        if old.is_null() {
            return Err(last_error("SetThreadDpiAwarenessContext(control)"));
        }
        let converted =
            unsafe { windows_sys::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut point) };
        unsafe {
            SetThreadDpiAwarenessContext(old);
        }
        if converted == 0 {
            return Err(last_error("ScreenToClient(control)"));
        }
        Ok(Point {
            x: point.x,
            y: point.y,
        })
    }

    pub fn inspect(handle: usize) -> Result<Self> {
        let _dpi = DpiContext::enter()?;
        let hwnd = handle as HWND;
        if unsafe {
            IsWindow(hwnd) == 0
                || IsWindowVisible(hwnd) == 0
                || IsIconic(hwnd) != 0
                || GetAncestor(hwnd, GA_ROOT) != hwnd
        } {
            return Err(Error::InvalidState(
                "target must be a visible, restored top-level window",
            ));
        }
        let mut process_id = 0;
        let thread_id = unsafe { GetWindowThreadProcessId(hwnd, &mut process_id) };
        if thread_id == 0 {
            return Err(last_error("GetWindowThreadProcessId"));
        }
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process_id) };
        if process.is_null() {
            return Err(last_error("OpenProcess"));
        }
        let (mut created, mut exited, mut kernel, mut user) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        let result =
            unsafe { GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user) };
        let error = (result == 0).then(|| last_error("GetProcessTimes"));
        unsafe {
            CloseHandle(process);
        }
        if let Some(error) = error {
            return Err(error);
        }
        let (mut bounds, mut client) = (RECT::default(), RECT::default());
        if unsafe { GetWindowRect(hwnd, &mut bounds) == 0 || GetClientRect(hwnd, &mut client) == 0 }
        {
            return Err(last_error("GetWindowRect/GetClientRect"));
        }
        let mut top_left = POINT {
            x: client.left,
            y: client.top,
        };
        let mut bottom_right = POINT {
            x: client.right,
            y: client.bottom,
        };
        if unsafe {
            ClientToScreen(hwnd, &mut top_left) == 0 || ClientToScreen(hwnd, &mut bottom_right) == 0
        } {
            return Err(last_error("ClientToScreen"));
        }
        Ok(Self {
            handle,
            process_id,
            thread_id,
            process_started: (created.dwHighDateTime as u64) << 32 | created.dwLowDateTime as u64,
            bounds: rectangle(bounds),
            client_bounds: Rect {
                left: top_left.x,
                top: top_left.y,
                right: bottom_right.x,
                bottom: bottom_right.y,
            },
        })
    }

    pub fn validate(&self) -> Result<()> {
        if Self::inspect(self.handle)? != *self {
            return Err(Error::InvalidState(
                "window identity or geometry changed; inspect again",
            ));
        }
        Ok(())
    }
}

/// Read-only discovery. Inaccessible/protected windows are omitted; inspect explicitly
/// to get their error. No window is activated, restored, moved or closed by discovery.
pub fn windows() -> Result<Vec<Window>> {
    let _dpi = DpiContext::enter()?;
    unsafe extern "system" fn collect(hwnd: HWND, data: LPARAM) -> i32 {
        let output = unsafe { &mut *(data as *mut Vec<Window>) };
        if let Ok(window) = Window::inspect(hwnd as usize) {
            output.push(window);
        }
        1
    }
    let mut output = Vec::new();
    if unsafe { EnumWindows(Some(collect), &mut output as *mut _ as LPARAM) } == 0 {
        return Err(last_error("EnumWindows"));
    }
    Ok(output)
}

pub(super) fn validate_hit(target: &Window, point: Point) -> Result<()> {
    if !target.client_bounds.contains(point) {
        return Err(Error::InvalidState(
            "pointer input must be inside the observed client area",
        ));
    }
    let hit = unsafe {
        WindowFromPoint(POINT {
            x: point.x,
            y: point.y,
        })
    };
    if hit.is_null() || unsafe { GetAncestor(hit, GA_ROOT) } != target.handle as HWND {
        return Err(Error::InvalidState(
            "target is occluded at the pointer location",
        ));
    }
    if unsafe { IsWindowEnabled(hit) } == 0 {
        return Err(Error::InvalidState("target control is disabled"));
    }
    Ok(())
}
