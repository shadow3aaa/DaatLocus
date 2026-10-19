use super::{DesktopState, InputReceipt, InputSurface, MouseAction, Window, WindowInputReceipt};
use crate::{Error, Point, Result};
use ::windows::{
    Win32::{Foundation::HWND, System::Com::*, UI::Accessibility::*},
    core::Interface,
};
use std::{marker::PhantomData, path::Path, rc::Rc, time::Duration};
use windows_sys::Win32::{Graphics::Gdi::ClientToScreen, UI::HiDpi::*};

fn uia_error(source: ::windows::core::Error) -> Error {
    Error::Automation {
        operation: "UI Automation",
        source,
    }
}
fn optional_element(
    value: ::windows::core::Result<IUIAutomationElement>,
) -> Result<Option<IUIAutomationElement>> {
    match value {
        Ok(value) => Ok(Some(value)),
        // The COM walker returns S_OK + null at a tree boundary. windows-core's
        // nonnullable projection represents precisely that as Error::empty().
        Err(error) if error.code().is_ok() => Ok(None),
        Err(error) => Err(uia_error(error)),
    }
}
struct Com;
impl Drop for Com {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}

pub(super) struct AutomationInput {
    root: Window,
    uia: IUIAutomation,
    activation: super::WindowHook,
    enabled: bool,
    _thread: PhantomData<Rc<()>>,
    // Release COM interfaces before balancing this thread's initialization.
    _com: Com,
}

enum Action {
    Invoke(IUIAutomationInvokePattern),
    Toggle(IUIAutomationTogglePattern),
    Select(IUIAutomationSelectionItemPattern),
    Expand(IUIAutomationExpandCollapsePattern, bool),
    Scroll(IUIAutomationScrollPattern, ScrollAmount, u32),
    Close(IUIAutomationWindowPattern),
}
impl Action {
    fn click_pattern(element: &IUIAutomationElement) -> Result<Option<Self>> {
        unsafe {
            Ok(Some(
                if let Ok(p) =
                    element.GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId)
                {
                    Self::Invoke(p)
                } else if let Ok(p) =
                    element.GetCurrentPatternAs::<IUIAutomationTogglePattern>(UIA_TogglePatternId)
                {
                    Self::Toggle(p)
                } else if let Ok(p) = element
                    .GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(
                        UIA_SelectionItemPatternId,
                    )
                {
                    Self::Select(p)
                } else if let Ok(p) = element
                    .GetCurrentPatternAs::<IUIAutomationExpandCollapsePattern>(
                        UIA_ExpandCollapsePatternId,
                    )
                {
                    let state = p.CurrentExpandCollapseState().map_err(uia_error)?;
                    Self::Expand(p, state == ExpandCollapseState_Expanded)
                } else {
                    return Ok(None);
                },
            ))
        }
    }
    fn execute(self) -> ::windows::core::Result<()> {
        unsafe {
            match self {
                Self::Invoke(p) => p.Invoke(),
                Self::Toggle(p) => p.Toggle(),
                Self::Select(p) => p.Select(),
                Self::Expand(p, expanded) => {
                    if expanded {
                        p.Collapse()
                    } else {
                        p.Expand()
                    }
                }
                Self::Close(p) => p.Close(),
                Self::Scroll(p, amount, count) => {
                    for _ in 0..count {
                        p.Scroll(ScrollAmount_NoAmount, amount)?;
                    }
                    Ok(())
                }
            }
        }
    }
}

impl AutomationInput {
    pub fn attach(root: &Window, dll: &Path) -> Result<Self> {
        root.validate()?;
        if root
            .input_surfaces()?
            .iter()
            .any(|s| s.process_id == std::process::id())
        {
            return Err(Error::InvalidState(
                "input controller must be outside the target process",
            ));
        }
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok() }.map_err(uia_error)?;
        let com = Com;
        let uia: IUIAutomation =
            unsafe { CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER) }
                .map_err(uia_error)?;
        let options: IUIAutomation2 = uia.cast().map_err(uia_error)?;
        // UIA otherwise focuses the target automatically before pattern calls,
        // even when the provider itself can perform the action in the background.
        unsafe { options.SetAutoSetFocus(false) }.map_err(uia_error)?;
        unsafe { options.SetConnectionTimeout(3000) }.map_err(uia_error)?;
        unsafe { options.SetTransactionTimeout(3000) }.map_err(uia_error)?;
        unsafe { uia.ElementFromHandle(HWND(root.handle as _)) }.map_err(uia_error)?;
        // A hosted UIA provider may request foreground activation when invoked.
        // Keep the host frame's CBT veto; no DLL enters restricted content.
        let activation = super::WindowHook::activation_guards(root, dll)?;
        Ok(Self {
            root: root.clone(),
            uia,
            activation,
            enabled: true,
            _thread: PhantomData,
            _com: com,
        })
    }
    pub fn input_enabled(&self) -> bool {
        self.enabled && self.activation.input_enabled()
    }
    pub fn veto_counts(&self) -> (u32, u32) {
        self.activation.veto_counts()
    }
    pub fn stop(&mut self) -> Result<()> {
        self.enabled = false;
        self.activation.stop()
    }
    pub fn prepare(&self, control: usize) -> Result<()> {
        if !self.input_enabled() {
            return Err(Error::InvalidState("window input has been revoked"));
        }
        self.root.validate()?;
        InputSurface::inspect(&self.root, control)?;
        if DesktopState::read()?.foreground == self.root.handle {
            return Err(Error::InvalidState(
                "target is foreground; release it for user takeover",
            ));
        }
        Ok(())
    }
    pub fn reobserve(&mut self) -> Result<Window> {
        if !self.enabled {
            return Err(Error::InvalidState("window input has been revoked"));
        }
        let current = Window::inspect(self.root.handle)?;
        if current.process_id != self.root.process_id
            || current.thread_id != self.root.thread_id
            || current.process_started != self.root.process_started
        {
            self.stop()?;
            return Err(Error::InvalidState("controlled window identity changed"));
        }
        self.root = current.clone();
        self.activation.reobserve()?;
        Ok(current)
    }
    fn configure_timeout(&self, timeout: Duration) -> Result<()> {
        if timeout.is_zero() || timeout > Duration::from_secs(10) {
            return Err(Error::InvalidState(
                "input timeout must be greater than zero and at most 10 seconds",
            ));
        }
        let options: IUIAutomation2 = self.uia.cast().map_err(uia_error)?;
        unsafe { options.SetConnectionTimeout(timeout.as_millis() as u32) }.map_err(uia_error)?;
        unsafe { options.SetTransactionTimeout(timeout.as_millis() as u32) }.map_err(uia_error)
    }
    fn element_at(&mut self, control: usize, local: Point) -> Result<IUIAutomationElement> {
        let hwnd = control as windows_sys::Win32::Foundation::HWND;
        let mut point = windows_sys::Win32::Foundation::POINT {
            x: local.x,
            y: local.y,
        };
        let old = unsafe { SetThreadDpiAwarenessContext(GetWindowDpiAwarenessContext(hwnd)) };
        if old.is_null() {
            return Err(super::last_error(
                "SetThreadDpiAwarenessContext(input surface)",
            ));
        }
        let converted = unsafe { ClientToScreen(hwnd, &mut point) };
        unsafe {
            SetThreadDpiAwarenessContext(old);
        }
        if converted == 0
            || unsafe { LogicalToPhysicalPointForPerMonitorDPI(hwnd, &mut point) } == 0
        {
            return Err(super::last_error("convert accessibility point"));
        }
        let frame = Window::inspect(unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::GetAncestor(
                hwnd,
                windows_sys::Win32::UI::WindowsAndMessaging::GA_ROOT,
            )
        } as usize)?;
        if !self.root.owns_control(frame.handle)
            || !frame.client_bounds.contains(Point {
                x: point.x,
                y: point.y,
            })
        {
            return Err(Error::InvalidState(
                "accessible point is outside the controlled window",
            ));
        }
        let mut target = self.element_in_window(&frame, point.x, point.y)?;
        if unsafe { target.CurrentIsOffscreen() }
            .map_err(uia_error)?
            .as_bool()
        {
            if let Err(error) = frame.raise_for_accessibility() {
                let _ = self.stop();
                return Err(error);
            }
            target = self.element_in_window(&frame, point.x, point.y)?;
            if unsafe { target.CurrentIsOffscreen() }
                .map_err(uia_error)?
                .as_bool()
            {
                return Err(Error::UnsupportedInput(
                    "accessible control remains offscreen",
                ));
            }
        }
        self.validate_element(&target)?;
        if !unsafe { target.CurrentIsEnabled() }
            .map_err(uia_error)?
            .as_bool()
        {
            return Err(Error::UnsupportedInput("accessible control is disabled"));
        }
        Ok(target)
    }
    fn element_in_window(&self, frame: &Window, x: i32, y: i32) -> Result<IUIAutomationElement> {
        let walker = unsafe { self.uia.RawViewWalker() }.map_err(uia_error)?;
        let mut current =
            unsafe { self.uia.ElementFromHandle(HWND(frame.handle as _)) }.map_err(uia_error)?;
        let mut remaining = 4096;
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        for _ in 0..128 {
            let mut child = optional_element(unsafe { walker.GetFirstChildElement(&current) })?;
            let mut next = None;
            while let Some(element) = child {
                if remaining == 0 || std::time::Instant::now() >= deadline {
                    return Err(Error::Timeout("accessibility window hit test"));
                }
                remaining -= 1;
                let rect = unsafe { element.CurrentBoundingRectangle() }.map_err(uia_error)?;
                if rect.left <= x && x < rect.right && rect.top <= y && y < rect.bottom {
                    self.validate_element(&element)?;
                    next = Some(element);
                    break;
                }
                child = optional_element(unsafe { walker.GetNextSiblingElement(&element) })?;
            }
            match next {
                Some(element) => current = element,
                None => {
                    frame.validate()?;
                    return Ok(current);
                }
            }
        }
        Err(Error::InvalidState(
            "accessible hit test exceeded its depth bound",
        ))
    }
    fn validate_element(&self, element: &IUIAutomationElement) -> Result<()> {
        let pid = unsafe { element.CurrentProcessId() }.map_err(uia_error)? as u32;
        if !self
            .root
            .input_surfaces()?
            .iter()
            .any(|s| s.process_id == pid)
        {
            return Err(Error::InvalidState(
                "accessible element is outside the controlled processes",
            ));
        }
        let walker = unsafe { self.uia.RawViewWalker() }.map_err(uia_error)?;
        let mut current = element.clone();
        for _ in 0..256 {
            let native = unsafe { current.CurrentNativeWindowHandle() }
                .map_err(uia_error)?
                .0 as usize;
            // Owned popup roots may sit beside the main window in the UIA tree.
            // Establish membership through a real HWND, never PID proximity.
            if native != 0 && self.root.owns_control(native) {
                InputSurface::inspect(&self.root, native)?;
                return Ok(());
            }
            current = unsafe { walker.GetParentElement(&current) }.map_err(uia_error)?;
        }
        Err(Error::InvalidState(
            "accessible element is outside the controlled window",
        ))
    }
    fn perform(
        &mut self,
        element: &IUIAutomationElement,
        action: Action,
    ) -> Result<WindowInputReceipt> {
        self.prepare(self.root.handle)?;
        self.validate_element(element)?;
        let before = DesktopState::read()?;
        if before.capture != 0 {
            return Err(Error::UnsupportedInput(
                "release the existing pointer capture before accessibility input",
            ));
        }
        // A COM failure/timeout or failed post-effect observation is uncertain.
        // Keep the lease disabled, and never send the same action through Hook.
        self.enabled = false;
        action.execute().map_err(uia_error)?;
        let after = DesktopState::read()?;
        let desktop = InputReceipt { before, after };
        self.enabled = !desktop.focus_changed() && !desktop.capture_changed();
        Ok(WindowInputReceipt { desktop })
    }
    pub fn mouse(
        &mut self,
        control: usize,
        action: MouseAction,
        local: Point,
        timeout: Duration,
    ) -> Result<WindowInputReceipt> {
        self.prepare(control)?;
        self.configure_timeout(timeout)?;
        let mut element = self.element_at(control, local)?;
        let action = match action {
            MouseAction::ClickLeft => {
                let walker = unsafe { self.uia.RawViewWalker() }.map_err(uia_error)?;
                let frame = unsafe {
                    windows_sys::Win32::UI::WindowsAndMessaging::GetAncestor(
                        control as _,
                        windows_sys::Win32::UI::WindowsAndMessaging::GA_ROOT,
                    )
                };
                let root = unsafe { self.uia.ElementFromHandle(HWND(frame)) }.map_err(uia_error)?;
                let mut pattern = None;
                // Raw trees can expose a button's decorative text as the leaf.
                // Choose its nearest actionable ancestor, stopping at this HWND.
                for _ in 0..64 {
                    self.validate_element(&element)?;
                    if !unsafe { element.CurrentIsEnabled() }
                        .map_err(uia_error)?
                        .as_bool()
                    {
                        return Err(Error::UnsupportedInput("accessible control is disabled"));
                    }
                    if let Some(action) = Action::click_pattern(&element)? {
                        pattern = Some(action);
                        break;
                    }
                    if unsafe { self.uia.CompareElements(&root, &element) }
                        .map_err(uia_error)?
                        .as_bool()
                    {
                        break;
                    }
                    let Some(parent) =
                        optional_element(unsafe { walker.GetParentElement(&element) })?
                    else {
                        break;
                    };
                    element = parent;
                }
                pattern.ok_or(Error::UnsupportedInput(
                    "this location exposes no accessibility click pattern",
                ))?
            }
            MouseAction::Wheel(delta) => {
                if delta == 0 || delta % 120 != 0 || delta.unsigned_abs() / 120 > 32 {
                    return Err(Error::UnsupportedInput(
                        "accessibility scrolling requires 1..=32 whole wheel steps",
                    ));
                }
                let walker = unsafe { self.uia.RawViewWalker() }.map_err(uia_error)?;
                let mut current = element.clone();
                let mut pattern = None;
                for _ in 0..64 {
                    self.validate_element(&current)?;
                    if let Ok(p) = unsafe {
                        current
                            .GetCurrentPatternAs::<IUIAutomationScrollPattern>(UIA_ScrollPatternId)
                    } && unsafe { p.CurrentVerticallyScrollable() }
                        .map_err(uia_error)?
                        .as_bool()
                    {
                        pattern = Some(p);
                        break;
                    }
                    if unsafe { current.CurrentNativeWindowHandle() }
                        .map_err(uia_error)?
                        .0 as usize
                        == self.root.handle
                    {
                        break;
                    }
                    current = unsafe { walker.GetParentElement(&current) }.map_err(uia_error)?;
                }
                let p = pattern.ok_or(Error::UnsupportedInput(
                    "this location exposes no vertical accessibility scroll pattern",
                ))?;
                Action::Scroll(
                    p,
                    if delta > 0 {
                        ScrollAmount_SmallDecrement
                    } else {
                        ScrollAmount_SmallIncrement
                    },
                    u32::from(delta.unsigned_abs() / 120),
                )
            }
            MouseAction::Move => {
                return Err(Error::UnsupportedInput(
                    "accessibility input does not synthesize pointer movement",
                ));
            }
        };
        self.perform(&element, action)
    }
    pub fn request_close(
        &mut self,
        window: usize,
        timeout: Duration,
    ) -> Result<WindowInputReceipt> {
        self.prepare(window)?;
        self.configure_timeout(timeout)?;
        let element =
            unsafe { self.uia.ElementFromHandle(HWND(window as _)) }.map_err(uia_error)?;
        self.validate_element(&element)?;
        let pattern = unsafe {
            element.GetCurrentPatternAs::<IUIAutomationWindowPattern>(UIA_WindowPatternId)
        }
        .map_err(|_| Error::UnsupportedInput("window exposes no accessibility close pattern"))?;
        self.perform(&element, Action::Close(pattern))
    }
}
impl Drop for AutomationInput {
    fn drop(&mut self) {
        self.enabled = false;
        let _ = self.activation.stop();
    }
}
