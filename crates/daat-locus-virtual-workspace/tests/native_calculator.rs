#![cfg(windows)]

use ::windows::{
    Win32::{
        Foundation::{CloseHandle, HWND},
        System::{Com::*, Variant::VARIANT},
        UI::{Accessibility::*, Shell::*},
    },
    core::w,
};
use daat_locus_virtual_workspace::{
    DesktopState, InputBackend, KeyModifiers, MouseAction, Point, SharedDisplay, VirtualWorkspace,
    Window, windows,
};
use std::{
    error::Error,
    path::Path,
    time::{Duration, Instant},
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

type Result<T> = std::result::Result<T, Box<dyn Error>>;
const TIMEOUT: Duration = Duration::from_secs(3);

struct Human {
    hwnd: usize,
    thread_id: u32,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Human {
    fn create() -> Result<Self> {
        let (tx, rx) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || unsafe {
            let hwnd = CreateWindowExW(
                0,
                w!("EDIT").as_ptr(),
                w!("human baseline").as_ptr(),
                WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                40,
                50,
                420,
                240,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            );
            if hwnd.is_null() {
                let _ = tx.send((0, 0));
                return;
            }
            SetForegroundWindow(hwnd);
            windows_sys::Win32::UI::Input::KeyboardAndMouse::SetFocus(hwnd);
            let _ = tx.send((
                hwnd as usize,
                windows_sys::Win32::System::Threading::GetCurrentThreadId(),
            ));
            let mut message = MSG::default();
            while GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) > 0 {
                if message.message == WM_APP + 91 {
                    // Join only our disposable Calculator's queue during setup.
                    // Detach before measuring input; never join a user's app.
                    let foreground = GetForegroundWindow();
                    let own_calculator = Window::inspect(message.wParam)
                        .is_ok_and(|window| window.owns_control(foreground as usize));
                    let foreground_thread =
                        GetWindowThreadProcessId(foreground, std::ptr::null_mut());
                    let current_thread =
                        windows_sys::Win32::System::Threading::GetCurrentThreadId();
                    let attached = own_calculator
                        && foreground_thread != 0
                        && windows_sys::Win32::System::Threading::AttachThreadInput(
                            current_thread,
                            foreground_thread,
                            1,
                        ) != 0;
                    SetForegroundWindow(hwnd);
                    windows_sys::Win32::UI::Input::KeyboardAndMouse::SetFocus(hwnd);
                    if attached {
                        windows_sys::Win32::System::Threading::AttachThreadInput(
                            current_thread,
                            foreground_thread,
                            0,
                        );
                    }
                    continue;
                }
                TranslateMessage(&message);
                DispatchMessageW(&message);
                if IsWindow(hwnd) == 0 {
                    break;
                }
            }
        });
        let (hwnd, thread_id) = rx.recv_timeout(TIMEOUT)?;
        if hwnd == 0 {
            return Err("could not create human fixture".into());
        }
        Ok(Self {
            hwnd,
            thread_id,
            thread: Some(thread),
        })
    }
}
impl Drop for Human {
    fn drop(&mut self) {
        unsafe {
            PostMessageW(self.hwnd as _, WM_CLOSE, 0, 0);
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
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

// This guard only closes the new frame whose identity was observed after launch.
// Never kill ApplicationFrameHost or a pre-existing Calculator process/window.
struct Calculator(Window);
impl Drop for Calculator {
    fn drop(&mut self) {
        if let Ok(current) = Window::inspect(self.0.handle)
            && current.process_id == self.0.process_id
            && current.process_started == self.0.process_started
            && current.thread_id == self.0.thread_id
        {
            unsafe {
                PostMessageW(current.handle as _, WM_CLOSE, 0, 0);
            }
        }
    }
}

fn element(
    uia: &IUIAutomation,
    root: &IUIAutomationElement,
    id: &str,
) -> Result<IUIAutomationElement> {
    let condition =
        unsafe { uia.CreatePropertyCondition(UIA_AutomationIdPropertyId, &VARIANT::from(id))? };
    Ok(unsafe { root.FindFirst(TreeScope_Descendants, &condition)? })
}

fn assert_desktop(expected: DesktopState) -> Result<()> {
    assert_eq!(
        DesktopState::read()?,
        expected,
        "Calculator input changed the human desktop"
    );
    Ok(())
}

fn wait_result(uia: &IUIAutomation, root: &IUIAutomationElement, expected: &str) -> Result<()> {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let name = unsafe { element(uia, root, "CalculatorResults")?.CurrentName()? }.to_string();
        // Results has a localized prefix. Arithmetic digits are the independent oracle.
        let digits: String = name.chars().filter(char::is_ascii_digit).collect();
        if digits == expected {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "Calculator did not apply input: expected {expected}, display={name:?}"
            )
            .into());
        }
        std::thread::sleep(Duration::from_millis(30));
    }
}

fn calculator_case(
    check: impl FnOnce(
        &VirtualWorkspace,
        &Window,
        &Path,
        &IUIAutomation,
        &IUIAutomationElement,
        DesktopState,
    ) -> Result<()>,
) -> Result<()> {
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
    }
    let _com = Com;
    let uia: IUIAutomation =
        unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)? };
    let dll = std::env::current_exe()?
        .parent()
        .ok_or("test directory missing")?
        .parent()
        .ok_or("profile directory missing")?
        .join("daat_locus_workspace_hook.dll");
    assert!(
        dll.is_file(),
        "build daat-locus-workspace-hook in the test profile first"
    );
    let service = SharedDisplay::new(Default::default())?;
    let region = service.reserve(640, 800)?;
    let workspace = VirtualWorkspace::attach(&region.display().id)?;
    let existing = windows()?;
    let mut launch = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_FLAG_NO_UI,
        lpFile: w!("shell:AppsFolder\\Microsoft.WindowsCalculator_8wekyb3d8bbwe!App"),
        nShow: SW_SHOWNOACTIVATE,
        ..Default::default()
    };
    unsafe {
        ShellExecuteExW(&mut launch)?;
    }
    if !launch.hProcess.is_invalid() {
        unsafe {
            CloseHandle(launch.hProcess)?;
        }
    }
    // Activation can expose a temporary splash frame before the XAML window.
    std::thread::sleep(Duration::from_millis(900));
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut stable: Option<(Window, Instant)> = None;
    let mut calculator = loop {
        if let Some(window) = windows()?.into_iter().find(|window| {
            !existing.iter().any(|old| old.handle == window.handle)
                && unsafe { uia.ElementFromHandle(HWND(window.handle as _)) }
                    .ok()
                    .is_some_and(|root| {
                        element(&uia, &root, "CalculatorResults").is_ok_and(|result| {
                            unsafe { result.CurrentIsOffscreen() }
                                .is_ok_and(|offscreen| !offscreen.as_bool())
                        })
                    })
        }) {
            if let Some((previous, since)) = &stable
                && previous == &window
            {
                if since.elapsed() >= Duration::from_millis(500) && window.validate().is_ok() {
                    break Calculator(window);
                }
            } else {
                stable = Some((window, Instant::now()));
            }
        } else {
            stable = None;
        }
        assert!(
            Instant::now() < deadline,
            "Calculator did not create a new accessible window; existing windows are never reused"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    let area = workspace.display().bounds;
    // Calculator remembers its last window mode. Restore only the new disposable
    // test window before placement; input/focus measurements start much later.
    let hwnd = calculator.0.handle as windows_sys::Win32::Foundation::HWND;
    if unsafe { IsZoomed(hwnd) } != 0 {
        unsafe {
            ShowWindowAsync(hwnd, SW_RESTORE);
        }
        let deadline = Instant::now() + TIMEOUT;
        while unsafe { IsZoomed(hwnd) } != 0 {
            assert!(
                Instant::now() < deadline,
                "could not restore disposable Calculator's maximized state"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    calculator.0 = Window::inspect(calculator.0.handle)?;
    println!(
        "Calculator before placement={:?}, workspace={area:?}",
        calculator.0
    );
    let destination = region.bounds();
    calculator.0 = calculator.0.relocate(destination).map_err(|error| {
        format!(
            "{error}; requested={destination:?}, observed={:?}",
            Window::inspect(calculator.0.handle)
        )
    })?;
    // Prepare our disposable fixture only. Calculator can activate even when its
    // launch hint says otherwise. Prevent activation during its restore, then
    // restore the original style BEFORE attaching input or measuring behavior.
    let hwnd = calculator.0.handle as windows_sys::Win32::Foundation::HWND;
    let original_style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) };
    unsafe {
        SetWindowLongPtrW(
            hwnd,
            GWL_EXSTYLE,
            original_style | WS_EX_NOACTIVATE as isize,
        );
        ShowWindowAsync(hwnd, SW_MINIMIZE);
    }
    let deadline = Instant::now() + TIMEOUT;
    while unsafe { IsIconic(hwnd) } == 0 || DesktopState::read()?.foreground == calculator.0.handle
    {
        assert!(
            Instant::now() < deadline,
            "could not minimize disposable Calculator for test setup"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let human = Human::create()?;
    unsafe {
        ShowWindowAsync(hwnd, SW_SHOWNOACTIVATE);
    }
    let deadline = Instant::now() + TIMEOUT;
    while unsafe { IsIconic(hwnd) } != 0 {
        assert!(
            Instant::now() < deadline,
            "could not restore disposable Calculator for test setup"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(200));
    unsafe {
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, original_style);
    }
    assert_eq!(
        unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) },
        original_style
    );
    std::thread::sleep(Duration::from_millis(600));
    // Activating our own fixture is test preparation, never a restoration of a
    // user's window after an input effect. Do not repair focus during actions.
    let deadline = Instant::now() + TIMEOUT;
    while !DesktopState::read().is_ok_and(|state| !calculator.0.owns_control(state.foreground)) {
        unsafe {
            PostThreadMessageW(human.thread_id, WM_APP + 91, calculator.0.handle, 0);
        }
        assert!(
            Instant::now() < deadline,
            "Windows denied the human fixture foreground prerequisite"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let baseline = DesktopState::read()?;
    assert!(!calculator.0.owns_control(baseline.foreground));
    assert_ne!(
        baseline.foreground, calculator.0.handle,
        "Calculator must be in the background before input; no focus restoration is performed"
    );
    assert_eq!(
        baseline.capture, 0,
        "release any physical mouse drag before testing"
    );
    let frame = Window::inspect(calculator.0.handle)?;
    region.validate()?;
    frame.validate_unoccluded()?;
    let surfaces = frame.input_surfaces()?;
    assert!(
        surfaces.iter().any(|s| s.process_id != frame.process_id),
        "this regression needs Calculator's hosted cross-process content"
    );
    println!("Calculator frame={frame:?}, input surfaces={surfaces:?}");
    let root = unsafe { uia.ElementFromHandle(HWND(frame.handle as _))? };
    let result = check(&workspace, &frame, &dll, &uia, &root, baseline);
    let handle = calculator.0.handle;
    drop(calculator);
    let deadline = Instant::now() + TIMEOUT;
    while unsafe { IsWindow(handle as _) } != 0 {
        assert!(
            Instant::now() < deadline,
            "test-created Calculator did not close"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    workspace.close()?;
    result
}

/// UI Automation only reads button bounds and arithmetic results. All effects use
/// the production workspace input API, so a successful dispatch alone cannot pass.
#[test]
#[ignore = "requires installed Calculator with Store-only DLL policy, signed IDD, matching Hook DLL, unlocked desktop and stationary cursor"]
fn calculator_cross_process_input_preserves_human_desktop_and_revokes_on_handoff() -> Result<()> {
    calculator_case(|workspace, frame, dll, uia, root, baseline| {
        let surfaces = frame.input_surfaces()?;
        let mut hook = workspace.input(frame, dll)?;
        assert_eq!(hook.backend(), InputBackend::Automation);
        for (id, result) in [
            ("clearButton", "0"),
            ("num7Button", "7"),
            ("plusButton", "7"),
            ("num5Button", "5"),
            ("equalButton", "12"),
        ] {
            let bounds = unsafe { element(uia, root, id)?.CurrentBoundingRectangle()? };
            let current = hook.reobserve()?;
            let point = Point {
                x: (bounds.left + bounds.right) / 2 - current.client_bounds.left,
                y: (bounds.top + bounds.bottom) / 2 - current.client_bounds.top,
            };
            let (control, local) = current.input_at(point)?;
            let receipt = hook.mouse(control, MouseAction::ClickLeft, local, TIMEOUT)?;
            println!("{id}: vetoes={:?}", hook.veto_counts());
            assert!(
                !receipt.desktop.focus_changed()
                    && !receipt.desktop.cursor_changed()
                    && !receipt.desktop.capture_changed(),
                "desktop changed on {id}: {:?}",
                receipt.desktop
            );
            wait_result(uia, root, result)?;
            assert_desktop(baseline)?;
        }
        // Accessibility must not pretend that arbitrary keys, caret insertion,
        // or dragging worked. The displayed arithmetic must remain unchanged.
        let content = surfaces
            .iter()
            .find(|s| s.process_id != frame.process_id)
            .ok_or("content surface missing")?
            .handle;
        assert!(matches!(
            hook.key(content, 0x1B, KeyModifiers::default(), TIMEOUT),
            Err(daat_locus_virtual_workspace::Error::UnsupportedInput(_))
        ));
        assert!(matches!(
            hook.text(content, "8", TIMEOUT),
            Err(daat_locus_virtual_workspace::Error::UnsupportedInput(_))
        ));
        assert!(hook.input_enabled());
        wait_result(uia, root, "12")?;
        assert_desktop(baseline)?;
        hook.stop()?;
        assert!(!hook.input_enabled());
        assert!(
            hook.text(content, "9", TIMEOUT).is_err(),
            "revoked route accepted input"
        );
        assert!(
            hook.mouse(
                content,
                MouseAction::ClickLeft,
                Point { x: 100, y: 100 },
                TIMEOUT
            )
            .is_err()
        );
        wait_result(uia, root, "12")?;
        assert_desktop(baseline)?;
        drop(hook);
        Ok(())
    })
}

#[test]
#[ignore = "requires Calculator with Microsoft/Store-only DLL policy, installed signed IDD, matching Hook DLL, unlocked desktop and stationary cursor"]
fn calculator_signature_policy_is_reported_before_input_and_partial_leases_are_released()
-> Result<()> {
    use daat_locus_virtual_workspace::Error as WorkspaceError;
    calculator_case(|workspace, frame, dll, _, _, baseline| {
        // Retrying the *setup* is safe: no input was dispatched. A leaked frame
        // lease would make the second result an existing-lease error instead.
        for _ in 0..2 {
            let error = match workspace.hook(frame, dll) {
                Ok(mut hook) => {
                    hook.stop()?;
                    return Err("Calculator signature policy prerequisite is absent".into());
                }
                Err(error) => error,
            };
            assert!(
                matches!(error,
                    WorkspaceError::InputSurface { process_id, source, .. }
                        if process_id != frame.process_id && matches!(*source, WorkspaceError::HookSignaturePolicy)
                ),
                "expected a pre-input signature-policy failure"
            );
            assert_desktop(baseline)?;
        }
        Ok(())
    })
}
