use super::{
    InputReceipt, KeyModifiers, MouseAction, Window, WindowHook, automation::AutomationInput,
};
use crate::{Error, Point, Rect, Result};
use std::{path::Path, time::Duration};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputBackend {
    Hook,
    Automation,
}

pub struct WindowInputReceipt {
    pub desktop: InputReceipt,
}

/// One visual-window lease. The backend is chosen at attachment, never by
/// retrying an operation whose effects may already have started.
pub struct WindowInput {
    backend: Backend,
}
enum Backend {
    Hook(WindowHook),
    Automation(AutomationInput),
}

pub(super) fn signature_restriction(error: &Error) -> bool {
    match error {
        Error::HookSignaturePolicy => true,
        Error::InputSurface { source, .. } => signature_restriction(source),
        _ => false,
    }
}

impl WindowInput {
    pub fn attach(target: &Window, dll: impl AsRef<Path>) -> Result<Self> {
        let dll = dll.as_ref();
        let backend = match WindowHook::attach(target, dll) {
            Ok(hook) => Backend::Hook(hook),
            Err(error) if signature_restriction(&error) => {
                Backend::Automation(AutomationInput::attach(target, dll)?)
            }
            Err(error) => return Err(error),
        };
        Ok(Self { backend })
    }
    /// Explicit accessibility mode for native diagnostics or callers that have
    /// selected semantic controls. It retains the frame activation guard.
    pub fn attach_automation(target: &Window, dll: impl AsRef<Path>) -> Result<Self> {
        Ok(Self {
            backend: Backend::Automation(AutomationInput::attach(target, dll.as_ref())?),
        })
    }
    pub fn backend(&self) -> InputBackend {
        match self.backend {
            Backend::Hook(_) => InputBackend::Hook,
            Backend::Automation(_) => InputBackend::Automation,
        }
    }
    pub fn supported_actions(&self) -> &'static [&'static str] {
        if !self.input_enabled() {
            return &["give_window"];
        }
        match self.backend {
            Backend::Hook(_) => &[
                "click_left",
                "literal_unicode",
                "shift_control_keys",
                "scroll_vertical",
                "drag_left",
                "close_window",
                "give_window",
                "adjust_window",
            ],
            Backend::Automation(_) => &[
                "click_left",
                "scroll_vertical",
                "close_window",
                "give_window",
                "adjust_window",
            ],
        }
    }
    pub fn input_enabled(&self) -> bool {
        match &self.backend {
            Backend::Hook(h) => h.input_enabled(),
            Backend::Automation(a) => a.input_enabled(),
        }
    }
    pub fn veto_counts(&self) -> (u32, u32) {
        match &self.backend {
            Backend::Hook(h) => h.veto_counts(),
            Backend::Automation(a) => a.veto_counts(),
        }
    }
    pub fn prepare(&mut self, control: usize) -> Result<()> {
        match &mut self.backend {
            Backend::Hook(h) => h.prepare(control),
            Backend::Automation(a) => a.prepare(control),
        }
    }
    pub fn reobserve(&mut self) -> Result<Window> {
        match &mut self.backend {
            Backend::Hook(h) => h.reobserve(),
            Backend::Automation(a) => a.reobserve(),
        }
    }
    /// Native window management under this lease's activation guards. Never
    /// restores foreground/cursor after a failure or retries placement.
    pub fn place_window(&mut self, target: &Window, bounds: Rect) -> Result<Window> {
        use windows_sys::Win32::UI::WindowsAndMessaging::{IsZoomed, SW_RESTORE, ShowWindowAsync};
        self.reobserve()?;
        self.prepare(target.handle)?;
        target.validate()?;
        let before = super::DesktopState::read()?;
        if target.owns_control(before.foreground) || before.capture != 0 {
            return Err(Error::InvalidState(
                "window adjustment requires a background target and no active mouse capture",
            ));
        }
        let result = (|| {
            if unsafe { IsZoomed(target.handle as _) } != 0 {
                if unsafe { ShowWindowAsync(target.handle as _, SW_RESTORE) } == 0 {
                    return Err(super::last_error("restore controlled window"));
                }
                let deadline = std::time::Instant::now() + Duration::from_secs(3);
                while unsafe { IsZoomed(target.handle as _) } != 0 {
                    if std::time::Instant::now() >= deadline {
                        return Err(Error::Timeout("restore controlled window"));
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
            let current = Window::inspect(target.handle)?;
            if current.process_id != target.process_id
                || current.thread_id != target.thread_id
                || current.process_started != target.process_started
            {
                return Err(Error::InvalidState(
                    "window identity changed before adjustment",
                ));
            }
            current.relocate(bounds)
        })();
        let after = super::DesktopState::read();
        if !after.as_ref().is_ok_and(|after| {
            before.foreground == after.foreground
                && before.focus == after.focus
                && before.capture == after.capture
                && before.cursor == after.cursor
        }) {
            let _ = self.stop();
            return Err(Error::InvalidState(
                "window adjustment changed foreground/focus/capture/cursor; input stopped",
            ));
        }
        let moved = result?;
        self.reobserve()?;
        Ok(moved)
    }
    pub fn mouse(
        &mut self,
        control: usize,
        action: MouseAction,
        local: Point,
        timeout: Duration,
    ) -> Result<WindowInputReceipt> {
        match &mut self.backend {
            Backend::Hook(h) => h
                .mouse(control, action, local, timeout)
                .map(|r| WindowInputReceipt { desktop: r.desktop }),
            Backend::Automation(a) => a.mouse(control, action, local, timeout),
        }
    }
    pub fn text(
        &mut self,
        control: usize,
        text: &str,
        timeout: Duration,
    ) -> Result<WindowInputReceipt> {
        self.prepare(control)?;
        match &mut self.backend {
            Backend::Hook(h) => h
                .text(control, text, timeout)
                .map(|r| WindowInputReceipt { desktop: r.desktop }),
            Backend::Automation(_) => Err(Error::UnsupportedInput(
                "this accessibility backend does not support caret insertion; use exposed clickable controls",
            )),
        }
    }
    pub fn key(
        &mut self,
        control: usize,
        key: u16,
        modifiers: KeyModifiers,
        timeout: Duration,
    ) -> Result<WindowInputReceipt> {
        self.prepare(control)?;
        match &mut self.backend {
            Backend::Hook(h) => h
                .key(control, key, modifiers, timeout)
                .map(|r| WindowInputReceipt { desktop: r.desktop }),
            Backend::Automation(_) => Err(Error::UnsupportedInput(
                "this accessibility backend does not support arbitrary key input; use exposed clickable controls",
            )),
        }
    }
    pub fn drag(
        &mut self,
        control: usize,
        path: &[Point],
        timeout: Duration,
    ) -> Result<WindowInputReceipt> {
        self.prepare(control)?;
        match &mut self.backend {
            Backend::Hook(h) => h
                .drag(control, path, timeout)
                .map(|r| WindowInputReceipt { desktop: r.desktop }),
            Backend::Automation(_) => Err(Error::UnsupportedInput(
                "this accessibility backend does not support pointer dragging",
            )),
        }
    }
    pub fn request_close(
        &mut self,
        window: usize,
        timeout: Duration,
    ) -> Result<WindowInputReceipt> {
        match &mut self.backend {
            Backend::Hook(h) => h
                .request_close(window, timeout)
                .map(|r| WindowInputReceipt { desktop: r.desktop }),
            Backend::Automation(a) => a.request_close(window, timeout),
        }
    }
    pub fn stop(&mut self) -> Result<()> {
        match &mut self.backend {
            Backend::Hook(h) => h.stop(),
            Backend::Automation(a) => a.stop(),
        }
    }
    pub fn close(mut self) -> Result<()> {
        self.stop()
    }
}
