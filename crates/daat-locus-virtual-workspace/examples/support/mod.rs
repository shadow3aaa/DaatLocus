use daat_locus_virtual_workspace::{
    ContactPhase, DesktopState, InputReceipt, Point, Rect, TouchDevice, VirtualWorkspace, Window,
    displays,
};
use std::{
    cell::Cell,
    error::Error,
    mem::size_of,
    ptr,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, WPARAM},
    Graphics::Gdi::{CreateSolidBrush, DeleteObject, HBRUSH},
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext},
        Input::Pointer::{GetPointerInfo, POINTER_INFO},
        WindowsAndMessaging::*,
    },
};

thread_local! {
    static AGENT: Cell<HWND> = const { Cell::new(ptr::null_mut()) };
    static DEFAULT_ACTIVATION: Cell<bool> = const { Cell::new(false) };
    static POINTER_DOWNS: Cell<u32> = const { Cell::new(0) };
    static POINTER_UPS: Cell<u32> = const { Cell::new(0) };
    static INVALID_POINTERS: Cell<u32> = const { Cell::new(0) };
    static ACTIVATIONS: Cell<u32> = const { Cell::new(0) };
    static START: Cell<bool> = const { Cell::new(false) };
}

unsafe extern "system" fn procedure(hwnd: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match message {
        WM_LBUTTONDOWN => {
            START.set(true);
        }
        WM_POINTERACTIVATE if hwnd == AGENT.get() && !DEFAULT_ACTIVATION.get() => {
            return PA_NOACTIVATE as LRESULT;
        }
        WM_ACTIVATE if hwnd == AGENT.get() && w as u16 != WA_INACTIVE as u16 => {
            ACTIVATIONS.set(ACTIVATIONS.get() + 1);
        }
        WM_POINTERDOWN | WM_POINTERUPDATE | WM_POINTERUP if hwnd == AGENT.get() => {
            let id = w as u32 & 0xffff;
            let mut info = POINTER_INFO::default();
            if unsafe { GetPointerInfo(id, &mut info) } == 0
                || info.hwndTarget != hwnd
                || info.pointerType != PT_TOUCH
            {
                INVALID_POINTERS.set(INVALID_POINTERS.get() + 1);
            }
            match message {
                WM_POINTERDOWN => POINTER_DOWNS.set(POINTER_DOWNS.get() + 1),
                WM_POINTERUP => POINTER_UPS.set(POINTER_UPS.get() + 1),
                _ => {}
            }
            println!(
                "received message=0x{message:x} id={id} target={:?} location=({}, {})",
                info.hwndTarget, info.ptPixelLocation.x, info.ptPixelLocation.y
            );
            // A pointer-aware control consumes its complete contact sequence, preventing
            // DefWindowProc from promoting this fixture's input into legacy mouse events.
            return 0;
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

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

struct Fixture {
    human: HWND,
    agent: HWND,
    class: Vec<u16>,
    brush: HBRUSH,
}

enum Backend {
    Workspace(Box<VirtualWorkspace>),
    Pointer(TouchDevice),
}
impl Backend {
    fn cancel(&mut self) -> daat_locus_virtual_workspace::Result<()> {
        match self {
            Self::Workspace(workspace) => workspace.cancel(),
            Self::Pointer(pointer) => pointer.cancel(),
        }
    }
    fn touch(
        &mut self,
        target: &Window,
        phase: ContactPhase,
        local: Point,
        bounds: Rect,
    ) -> daat_locus_virtual_workspace::Result<InputReceipt> {
        match self {
            Self::Workspace(workspace) => workspace.touch(target, phase, local),
            Self::Pointer(pointer) => {
                target.validate()?;
                let before = DesktopState::read()?;
                pointer.inject(phase, bounds.screen_point(local)?)?;
                Ok(InputReceipt {
                    before,
                    after: DesktopState::read()?,
                })
            }
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        AGENT.set(ptr::null_mut());
        unsafe {
            if IsWindow(self.agent) != 0 {
                DestroyWindow(self.agent);
            }
            if IsWindow(self.human) != 0 {
                DestroyWindow(self.human);
            }
            UnregisterClassW(self.class.as_ptr(), GetModuleHandleW(ptr::null()));
            DeleteObject(self.brush);
        }
    }
}

/// Uses exactly one process and GUI thread for both windows. No existing user window
/// is moved, injected into, modified, or closed. The lab's own pointer handler can
/// decline activation; --default-activation measures Windows' unmodified default.
pub fn run(
    display_name: &str,
    default_activation: bool,
    automated: bool,
    native_only: bool,
) -> Result<(), Box<dyn Error>> {
    run_with_options(
        display_name,
        LabOptions {
            default_activation,
            automated,
            native_only,
            ..Default::default()
        },
    )
}

#[derive(Default)]
pub struct LabOptions {
    pub default_activation: bool,
    pub automated: bool,
    pub native_only: bool,
    pub click_to_start: bool,
    pub noactivate_window: bool,
}

pub fn run_with_options(display_name: &str, options: LabOptions) -> Result<(), Box<dyn Error>> {
    let LabOptions {
        default_activation,
        automated,
        native_only,
        click_to_start,
        noactivate_window,
    } = options;
    struct Dpi(windows_sys::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT);
    impl Drop for Dpi {
        fn drop(&mut self) {
            unsafe {
                SetThreadDpiAwarenessContext(self.0);
            }
        }
    }
    let dpi = unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    if dpi.is_null() {
        return Err(std::io::Error::last_os_error().into());
    }
    let _dpi = Dpi(dpi);
    let active = displays()?;
    let display = active
        .iter()
        .find(|d| d.device_name == display_name)
        .ok_or("selected display is not active")?;
    let physical = active
        .iter()
        .find(|d| !d.indirect && !d.cloned)
        .ok_or("physical display required")?;
    let mut backend = if native_only {
        Backend::Pointer(TouchDevice::new()?)
    } else {
        Backend::Workspace(Box::new(VirtualWorkspace::attach(&display.id)?))
    };
    DEFAULT_ACTIVATION.set(default_activation);
    AGENT.set(ptr::null_mut());
    POINTER_DOWNS.set(0);
    POINTER_UPS.set(0);
    INVALID_POINTERS.set(0);
    ACTIVATIONS.set(0);
    START.set(false);
    let class = wide(&format!(
        "DaatLocusVirtualWorkspaceLab{}",
        std::process::id()
    ));
    let brush = unsafe { CreateSolidBrush(0x00332211) };
    if brush.is_null() {
        return Err(std::io::Error::last_os_error().into());
    }
    let mut fixture = Fixture {
        human: ptr::null_mut(),
        agent: ptr::null_mut(),
        class,
        brush,
    };
    let instance = unsafe { GetModuleHandleW(ptr::null()) };
    let class_info = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(procedure),
        hInstance: instance,
        hCursor: unsafe { LoadCursorW(ptr::null_mut(), IDC_ARROW) },
        hbrBackground: brush,
        lpszClassName: fixture.class.as_ptr(),
        ..Default::default()
    };
    if unsafe { RegisterClassExW(&class_info) } == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let agent_left = if native_only {
        display.bounds.right - 360
    } else {
        display.bounds.left + 20
    };
    let agent_rect = Rect {
        left: agent_left,
        top: display.bounds.top + 20,
        right: agent_left + 320,
        bottom: display.bounds.top + 260,
    };
    if !display.bounds.contains_rect(agent_rect) {
        return Err("workspace display must fit a 340x260 lab area".into());
    }
    unsafe {
        fixture.human = CreateWindowExW(
            0,
            fixture.class.as_ptr(),
            wide("Human: click here to start; keep using this window").as_ptr(),
            WS_OVERLAPPEDWINDOW,
            physical.bounds.left + 40,
            physical.bounds.top + 40,
            480,
            260,
            ptr::null_mut(),
            ptr::null_mut(),
            instance,
            ptr::null(),
        );
        fixture.agent = CreateWindowExW(
            if noactivate_window {
                WS_EX_NOACTIVATE
            } else {
                0
            },
            fixture.class.as_ptr(),
            wide("Agent: native WM_POINTER receiver").as_ptr(),
            WS_OVERLAPPEDWINDOW,
            agent_rect.left,
            agent_rect.top,
            320,
            240,
            ptr::null_mut(),
            ptr::null_mut(),
            instance,
            ptr::null(),
        );
    }
    if fixture.human.is_null() || fixture.agent.is_null() {
        return Err(std::io::Error::last_os_error().into());
    }
    AGENT.set(fixture.agent);
    unsafe {
        ShowWindow(fixture.human, SW_SHOWNOACTIVATE);
        ShowWindow(fixture.agent, SW_SHOWNOACTIVATE);
    }
    // Drive placement through the production backend, then allow the GUI queue to settle.
    let target = Window::inspect(fixture.agent as usize)?;
    if let Backend::Workspace(workspace) = &mut backend {
        workspace.place(&target, agent_rect)?;
    }
    if automated && !click_to_start {
        // Windows can deny foreground activation. Begin assertions only after
        // observing the actual foreground, rather than trusting an API receipt.
        unsafe { SetForegroundWindow(fixture.human) };
    }
    println!(
        "Both windows share PID {} and this GUI thread. Click the human window to start. Close either window to end.",
        std::process::id()
    );
    let mut message = MSG::default();
    let mut started = None;
    let mut step = 0;
    let mut baseline = None;
    let mut focus_changed = false;
    let mut cursor_changed = false;
    let mut deadline = Instant::now()
        + Duration::from_secs(if click_to_start {
            120
        } else if automated {
            8
        } else {
            300
        });
    let mut next_frame = Instant::now();
    let mut observed_color = false;
    let mut observed_pixel = None;
    let mut capture_time = Duration::ZERO;
    while Instant::now() < deadline {
        while unsafe { PeekMessageW(&mut message, ptr::null_mut(), 0, 0, PM_REMOVE) } != 0 {
            if message.message == WM_QUIT {
                return if automated {
                    Err("fixture closed before verification".into())
                } else {
                    Ok(())
                };
            }
            unsafe {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        if automated
            && !click_to_start
            && started.is_none()
            && unsafe { GetForegroundWindow() } == fixture.human
        {
            START.set(true);
        }
        if START.get() && started.is_none() {
            // A human deliberately selecting the fixture is a new observation boundary.
            // Placement may have observed the formerly focused application; do not carry
            // that baseline into the explicit human-started input experiment.
            if let Backend::Workspace(workspace) = &mut backend {
                **workspace = VirtualWorkspace::attach(&display.id)?;
            }
            baseline = Some(DesktopState::read()?);
            if automated && baseline.unwrap().foreground != fixture.human as usize {
                return Err("disposable human window did not become foreground".into());
            }
            started = Some(Instant::now());
            if automated {
                // Foreground establishment / a real user click has its own wait.
                // A late start must still receive the full assertion interval.
                deadline = Instant::now() + Duration::from_secs(8);
            }
        }
        if started.is_some() && !observed_color {
            let target = Window::inspect(fixture.agent as usize)?;
            if let Backend::Workspace(workspace) = &backend {
                let capture_started = Instant::now();
                let frame = workspace.capture()?;
                capture_time += capture_started.elapsed();
                // Interior background has an independently specified COLORREF 0x00332211.
                let x = (target.client_bounds.left - display.bounds.left + 20) as usize;
                let y = (target.client_bounds.top - display.bounds.top + 20) as usize;
                let offset = (y * frame.width as usize + x) * 4;
                observed_color =
                    frame.bgra.get(offset..offset + 4) == Some(&[0x33, 0x22, 0x11, 0xff]);
                observed_pixel = frame
                    .bgra
                    .get(offset..offset + 4)
                    .map(|pixel| <[u8; 4]>::try_from(pixel).unwrap());
            }
        }
        // OS delivery is asynchronous. Check after dispatch too, so a delayed activation
        // cannot be hidden by matching before/after snapshots around the injection call.
        if let Some(before) = baseline {
            let now = DesktopState::read()?;
            if before.foreground != now.foreground || before.focus != now.focus {
                focus_changed = true;
                backend.cancel()?;
                break;
            }
        }
        if started.is_some() && step < 5 && Instant::now() >= next_frame {
            let target = Window::inspect(fixture.agent as usize)?;
            let local = Point {
                x: target.client_bounds.left - display.bounds.left
                    + 50
                    + if step == 1 || step == 2 { 15 } else { 0 },
                y: target.client_bounds.top - display.bounds.top + 60,
            };
            let phase = [
                ContactPhase::Down,
                ContactPhase::Move,
                ContactPhase::Up,
                ContactPhase::Down,
                ContactPhase::Up,
            ][step];
            match backend.touch(&target, phase, local, display.bounds) {
                Ok(receipt) => {
                    focus_changed |= receipt.focus_changed();
                    cursor_changed |= receipt.cursor_changed();
                    println!(
                        "accepted {phase:?}: focus_changed={} cursor_changed={}",
                        receipt.focus_changed(),
                        receipt.cursor_changed()
                    );
                    if receipt.focus_changed() {
                        backend.cancel()?;
                        break;
                    }
                }
                Err(error) => {
                    backend.cancel()?;
                    return Err(error.into());
                }
            }
            step += 1;
            next_frame = Instant::now() + Duration::from_millis(80);
        }
        if step == 5 && Instant::now() > next_frame + Duration::from_millis(200) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    backend.cancel()?;
    if automated && started.is_none() {
        return Err(if click_to_start {
            "timed out waiting for a click inside the disposable human fixture"
        } else {
            "Windows did not activate the disposable human fixture; use --click-to-start from a focused local terminal"
        }.into());
    }
    let after = DesktopState::read()?;
    if let Some(before) = baseline {
        focus_changed |= before.foreground != after.foreground || before.focus != after.focus;
        cursor_changed |= before.cursor != after.cursor;
    }
    println!(
        "result: down={} up={} invalid_pointer_info={} agent_activations={} focus_changed={focus_changed} cursor_changed={cursor_changed} capture_color_verified={observed_color} observed_pixel={observed_pixel:?} capture_elapsed_ms={}",
        POINTER_DOWNS.get(),
        POINTER_UPS.get(),
        INVALID_POINTERS.get(),
        ACTIVATIONS.get(),
        capture_time.as_millis()
    );
    if automated && default_activation {
        if POINTER_DOWNS.get() == 0
            || INVALID_POINTERS.get() != 0
            || ACTIVATIONS.get() == 0
            || !focus_changed
        {
            return Err(
                "expected genuine pointer delivery and default window activation were not observed"
                    .into(),
            );
        }
    } else if automated
        && (step != 5
            || POINTER_DOWNS.get() != 2
            || POINTER_UPS.get() != 2
            || INVALID_POINTERS.get() != 0
            || ACTIVATIONS.get() != 0
            || focus_changed
            || cursor_changed
            || (!native_only && !observed_color))
    {
        return Err(
            "native pointer delivery, capture, or input preservation failed; see result".into(),
        );
    }
    Ok(())
}
