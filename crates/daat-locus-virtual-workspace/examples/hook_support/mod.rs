use daat_locus_virtual_workspace::{
    ContactPhase, DesktopState, KeyModifiers, MouseAction, Point, TouchDevice, VirtualWorkspace,
    Window, WindowHook, displays,
};
use std::{
    cell::Cell,
    error::Error,
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Child, Command, Stdio},
    ptr,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        Controls::*,
        HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext},
        Input::{
            KeyboardAndMouse::*,
            Pointer::{GetPointerInfo, POINTER_INFO},
        },
        WindowsAndMessaging::*,
    },
};

mod preview;
// Trackbar's TBM_GETPOS is WM_USER (commctrl.h); windows-sys omits that alias.
const TRACKBAR_GETPOS: u32 = WM_USER;

const QUERY: u32 = WM_APP + 40;
const ACTIVATE: u32 = WM_APP + 41;
const SET_FOCUS: u32 = WM_APP + 42;
const PAUSE: u32 = WM_APP + 43;
const CAPTURE: u32 = WM_APP + 44;
thread_local! {
    static HUMAN: Cell<HWND> = const { Cell::new(ptr::null_mut()) };
    static HUMAN_EDIT: Cell<HWND> = const { Cell::new(ptr::null_mut()) };
    static AGENT: Cell<HWND> = const { Cell::new(ptr::null_mut()) };
    static AGENT_EDIT: Cell<HWND> = const { Cell::new(ptr::null_mut()) };
    static SLIDER: Cell<HWND> = const { Cell::new(ptr::null_mut()) };
    static STARTED: Cell<bool> = const { Cell::new(false) };
    static ACTIVATIONS: Cell<u32> = const { Cell::new(0) };
    static DOWNS: Cell<u32> = const { Cell::new(0) };
    static UPS: Cell<u32> = const { Cell::new(0) };
    static INVALID_POINTERS: Cell<u32> = const { Cell::new(0) };
    static CLICKS: Cell<u32> = const { Cell::new(0) };
    static DELAY_CLICK: Cell<u32> = const { Cell::new(0) };
}
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

unsafe extern "system" fn procedure(hwnd: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if hwnd == HUMAN.get()
        && let Some(result) = preview::handle(hwnd, message, w, l)
    {
        return result;
    }
    match message {
        WM_COMMAND if hwnd == HUMAN.get() && w & 0xffff == 3 && w >> 16 == BN_CLICKED as usize => {
            STARTED.set(true);
            unsafe {
                SetFocus(HUMAN_EDIT.get());
            }
            return 0;
        }
        WM_SETFOCUS if hwnd == HUMAN.get() => unsafe {
            SetFocus(HUMAN_EDIT.get());
        },
        WM_ACTIVATE if hwnd == AGENT.get() && w as u16 != WA_INACTIVE as u16 => {
            ACTIVATIONS.set(ACTIVATIONS.get() + 1)
        }
        WM_POINTERDOWN | WM_POINTERUP if hwnd == AGENT.get() => {
            let mut info = POINTER_INFO::default();
            if unsafe { GetPointerInfo(w as u32 & 0xffff, &mut info) } == 0
                || info.hwndTarget != hwnd
                || info.pointerType != PT_TOUCH
            {
                INVALID_POINTERS.set(INVALID_POINTERS.get() + 1);
            }
            if message == WM_POINTERDOWN {
                DOWNS.set(DOWNS.get() + 1);
            } else {
                UPS.set(UPS.get() + 1);
            }
            // Deliberately fall through to default touch/mouse handling. No PA_NOACTIVATE.
        }
        WM_COMMAND
            if hwnd == AGENT.get() && w & 0xffff == 2 && (w >> 16) == BN_CLICKED as usize =>
        {
            let delay = DELAY_CLICK.replace(0);
            if delay != 0 {
                std::thread::sleep(Duration::from_millis(delay as u64));
            }
            CLICKS.set(CLICKS.get() + 1)
        }
        QUERY => {
            return match w {
                0 => ACTIVATIONS.get() as isize,
                1 => DOWNS.get() as isize,
                2 => UPS.get() as isize,
                3 => INVALID_POINTERS.get() as isize,
                4 => CLICKS.get() as isize,
                5 => {
                    let mut keys = [0u8; 256];
                    unsafe {
                        GetKeyboardState(keys.as_mut_ptr());
                    }
                    u32::from(keys[VK_SHIFT as usize]) as isize
                        | ((keys[VK_CONTROL as usize] as isize) << 8)
                }
                6 => unsafe { SendMessageW(SLIDER.get(), TRACKBAR_GETPOS, 0, 0) },
                7 | 8 => {
                    let mut rect = RECT::default();
                    unsafe {
                        SendMessageW(
                            SLIDER.get(),
                            TBM_GETTHUMBRECT,
                            0,
                            &mut rect as *mut _ as isize,
                        );
                    }
                    if w == 7 {
                        ((rect.left + rect.right) / 2) as isize
                    } else {
                        ((rect.top + rect.bottom) / 2) as isize
                    }
                }
                9 | 10 => {
                    let mut rect = RECT::default();
                    unsafe {
                        SendMessageW(
                            SLIDER.get(),
                            TBM_GETCHANNELRECT,
                            0,
                            &mut rect as *mut _ as isize,
                        );
                    }
                    if w == 9 {
                        rect.right as isize
                    } else {
                        rect.left as isize
                    }
                }
                11 => STARTED.get() as isize,
                _ => -1,
            };
        }
        ACTIVATE => {
            unsafe {
                SetForegroundWindow(hwnd);
            }
            return 0;
        }
        SET_FOCUS => {
            unsafe {
                SetFocus(AGENT_EDIT.get());
            }
            return 0;
        }
        PAUSE => {
            std::thread::sleep(Duration::from_millis(w as u64));
            return 0;
        }
        message if message == WM_APP + 45 => {
            DELAY_CLICK.set(w as u32);
            return 0;
        }
        message if message == WM_APP + 48 => {
            unsafe {
                ShowWindow(hwnd, SW_MAXIMIZE);
            }
            return 0;
        }
        CAPTURE => {
            if w != 0 {
                unsafe {
                    SetCapture(HUMAN_EDIT.get());
                }
            } else {
                unsafe {
                    ReleaseCapture();
                }
            }
            return 0;
        }
        WM_CLOSE => {
            if hwnd == HUMAN.get() {
                preview::detach(hwnd);
            }
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

pub fn fixture(
    agent_x: i32,
    agent_y: i32,
    preview_display: Option<&str>,
) -> Result<(), Box<dyn Error>> {
    unsafe {
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let module = unsafe { GetModuleHandleW(ptr::null()) };
    let controls = INITCOMMONCONTROLSEX {
        dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
        dwICC: ICC_BAR_CLASSES,
    };
    if unsafe { InitCommonControlsEx(&controls) } == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let class = wide("DaatLocusCrossProcessHookLab");
    let brush = unsafe { windows_sys::Win32::Graphics::Gdi::CreateSolidBrush(0x00332211) };
    let definition = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(procedure),
        hInstance: module,
        hCursor: unsafe { LoadCursorW(ptr::null_mut(), IDC_ARROW) },
        hbrBackground: brush,
        lpszClassName: class.as_ptr(),
        ..Default::default()
    };
    if unsafe { RegisterClassExW(&definition) } == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let physical = displays()?
        .into_iter()
        .find(|d| !d.indirect && !d.cloned)
        .ok_or("physical display required")?;
    let human = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            wide(if preview_display.is_some() {
                "Daat Locus — Virtual Workspace"
            } else {
                "Hook lab: human window"
            })
            .as_ptr(),
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            physical.bounds.left + 40,
            physical.bounds.top + 60,
            if preview_display.is_some() { 1050 } else { 380 },
            if preview_display.is_some() { 740 } else { 300 },
            ptr::null_mut(),
            ptr::null_mut(),
            module,
            ptr::null(),
        )
    };
    let agent = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            wide("Hook lab: background agent window").as_ptr(),
            WS_OVERLAPPEDWINDOW,
            agent_x,
            agent_y,
            if preview_display.is_some() { 720 } else { 380 },
            if preview_display.is_some() { 420 } else { 300 },
            ptr::null_mut(),
            ptr::null_mut(),
            module,
            ptr::null(),
        )
    };
    if human.is_null() || agent.is_null() {
        return Err(std::io::Error::last_os_error().into());
    }
    HUMAN.set(human);
    AGENT.set(agent);
    let mut edits = [ptr::null_mut(); 2];
    for (index, root) in [human, agent].iter().enumerate() {
        edits[index] = unsafe {
            CreateWindowExW(
                WS_EX_CLIENTEDGE,
                wide("EDIT").as_ptr(),
                wide(if index == 0 { "human baseline" } else { "" }).as_ptr(),
                WS_CHILD | WS_VISIBLE | WS_TABSTOP | ES_AUTOHSCROLL as u32,
                20,
                25,
                300,
                30,
                *root,
                1usize as _,
                module,
                ptr::null(),
            )
        };
    }
    HUMAN_EDIT.set(edits[0]);
    AGENT_EDIT.set(edits[1]);
    let button = unsafe {
        CreateWindowExW(
            0,
            wide("BUTTON").as_ptr(),
            wide("Native button").as_ptr(),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP,
            20,
            85,
            160,
            35,
            agent,
            2usize as _,
            module,
            ptr::null(),
        )
    };
    let slider = unsafe {
        CreateWindowExW(
            0,
            TRACKBAR_CLASSW,
            ptr::null(),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | TBS_AUTOTICKS,
            20,
            145,
            300,
            45,
            agent,
            4usize as _,
            module,
            ptr::null(),
        )
    };
    SLIDER.set(slider);
    unsafe {
        SendMessageW(slider, TBM_SETRANGE, 1, 100 << 16);
        SendMessageW(slider, TBM_SETPOS, 1, 20);
    }
    if edits.iter().any(|v| v.is_null()) || button.is_null() || slider.is_null() {
        return Err(std::io::Error::last_os_error().into());
    }
    if let Some(display) = preview_display {
        preview::attach(human, edits[0], display)?;
    }
    unsafe {
        ShowWindow(agent, SW_SHOWNOACTIVATE);
        ShowWindow(human, SW_SHOW);
        SetWindowPos(
            agent,
            HWND_TOP,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
        SetForegroundWindow(human);
        SetFocus(edits[0]);
    }
    println!(
        "READY {} {} {} {} {} {}",
        human as usize,
        agent as usize,
        edits[0] as usize,
        edits[1] as usize,
        button as usize,
        slider as usize
    );
    std::io::stdout().flush()?;
    let mut message = MSG::default();
    loop {
        let result = unsafe { GetMessageW(&mut message, ptr::null_mut(), 0, 0) };
        if result == -1 {
            return Err(std::io::Error::last_os_error().into());
        }
        if result == 0 {
            break;
        }
        unsafe {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    unsafe {
        DestroyWindow(agent);
        DestroyWindow(human);
        UnregisterClassW(class.as_ptr(), module);
        windows_sys::Win32::Graphics::Gdi::DeleteObject(brush);
    }
    Ok(())
}

struct Fixture(Child);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn send(hwnd: usize, message: u32, w: usize, l: isize) -> Result<usize, Box<dyn Error>> {
    let mut result = 0;
    if unsafe {
        SendMessageTimeoutW(
            hwnd as HWND,
            message,
            w,
            l,
            SMTO_ABORTIFHUNG | SMTO_BLOCK,
            2000,
            &mut result,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(result)
}
fn text(hwnd: usize) -> Result<String, Box<dyn Error>> {
    let mut text = [0u16; 2048];
    let length = send(hwnd, WM_GETTEXT, text.len(), text.as_mut_ptr() as isize)?;
    Ok(String::from_utf16(&text[..length])?)
}
fn verify_desktop(before: DesktopState, strict_cursor: bool) -> Result<(), Box<dyn Error>> {
    let after = DesktopState::read()?;
    if before.foreground != after.foreground
        || before.focus != after.focus
        || before.capture != after.capture
        || (strict_cursor && before.cursor != after.cursor)
    {
        return Err(format!("desktop changed: before={before:?}, after={after:?}").into());
    }
    Ok(())
}

fn stationary_baseline(human: usize, human_edit: usize) -> Result<DesktopState, Box<dyn Error>> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut previous = DesktopState::read()?;
    let mut steady_since = Instant::now();
    loop {
        let current = DesktopState::read()?;
        if current.foreground != human || current.focus != human_edit {
            return Err("human fixture lost focus before assertions".into());
        }
        if current != previous {
            steady_since = Instant::now();
        }
        if Instant::now() - steady_since >= Duration::from_millis(400) {
            return Ok(current);
        }
        if Instant::now() >= deadline {
            return Err("physical cursor did not remain stationary before assertions".into());
        }
        previous = current;
        std::thread::sleep(Duration::from_millis(20));
    }
}

pub fn lease_owner(target: usize, dll: PathBuf) -> Result<(), Box<dyn Error>> {
    let _hook = WindowHook::attach(&Window::inspect(target)?, dll)?;
    println!("LEASE READY");
    std::io::stdout().flush()?;
    std::thread::sleep(Duration::from_secs(30));
    Err("lease owner should have been killed by its controller".into())
}

pub fn run(
    dll: PathBuf,
    owned: bool,
    native_touch: bool,
    automated: bool,
    start_automatically: bool,
) -> Result<(), Box<dyn Error>> {
    unsafe {
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let workspace = if owned {
        Some(VirtualWorkspace::create(
            daat_locus_virtual_workspace::DisplayOptions {
                width: 1280,
                height: 720,
                ..Default::default()
            },
        )?)
    } else {
        None
    };
    let physical = displays()?
        .into_iter()
        .find(|d| !d.indirect && !d.cloned)
        .ok_or("physical display required")?;
    let (x, y) = match &workspace {
        Some(w) => (w.display().bounds.left + 30, w.display().bounds.top + 40),
        None => (physical.bounds.left + 500, physical.bounds.top + 100),
    };
    let mut command = Command::new(std::env::current_exe()?);
    if let Some(workspace) = &workspace {
        command
            .arg("--preview-display")
            .arg(&workspace.display().device_name);
    }
    let mut fixture = Fixture(
        command
            .args([
                "--fixture",
                "--agent-x",
                &x.to_string(),
                "--agent-y",
                &y.to_string(),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?,
    );
    let stdout = fixture.0.stdout.take().ok_or("missing fixture stdout")?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let result = BufReader::new(stdout).read_line(&mut line).map(|_| line);
        let _ = tx.send(result);
    });
    let ready = rx.recv_timeout(Duration::from_secs(8))??;
    let handles: Vec<usize> = ready
        .trim()
        .strip_prefix("READY ")
        .ok_or("fixture did not start")?
        .split_whitespace()
        .map(str::parse)
        .collect::<Result<_, _>>()?;
    if handles.len() != 6 {
        return Err("invalid fixture handshake".into());
    }
    let (human, agent, human_edit, agent_edit, button) =
        (handles[0], handles[1], handles[2], handles[3], handles[4]);
    let slider = handles[5];
    let verification = (|| -> Result<(), Box<dyn Error>> {
        if owned && !start_automatically {
            loop {
                if fixture.0.try_wait()?.is_some() {
                    return Ok(());
                }
                if send(human, QUERY, 11, 0)? != 0 {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        let target = Window::inspect(agent)?;
        let human_window = Window::inspect(human)?;
        if target.process_id != fixture.0.id() || target.thread_id != human_window.thread_id {
            return Err("fixture must contain two windows on one external GUI thread".into());
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while unsafe { GetForegroundWindow() } as usize != human {
            send(human, ACTIVATE, 0, 0)?;
            if Instant::now() > deadline {
                return Err("Windows denied fixture foreground activation; run from an interactive foreground terminal".into());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let baseline = DesktopState::read()?;
        if baseline.focus != human_edit {
            return Err("human edit must have actual keyboard focus".into());
        }
        let keyboard = send(agent, QUERY, 5, 0)?;
        if let Some(workspace) = &workspace {
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                let frame = workspace.capture()?;
                let x =
                    (target.client_bounds.left - workspace.display().bounds.left + 340) as usize;
                let y = (target.client_bounds.top - workspace.display().bounds.top + 220) as usize;
                let offset = (y * frame.width as usize + x) * 4;
                if frame.bgra.get(offset..offset + 4) == Some(&[0x33, 0x22, 0x11, 0xff]) {
                    break;
                }
                if Instant::now() > deadline {
                    return Err("owned display did not capture actual fixture pixels".into());
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            println!("owned IDD capture contains the fixture's expected BGRA pixels");
        }
        let baseline = if automated {
            stationary_baseline(human, human_edit)?
        } else {
            DesktopState::read()?
        };
        let mut hook = match &workspace {
            Some(w) => w.hook(&target, &dll)?,
            None => WindowHook::attach(&target, &dll)?,
        };
        if WindowHook::attach(&target, &dll).is_ok() {
            return Err("duplicate thread lease was accepted".into());
        }
        println!(
            "hot attach verified on external PID {}, GUI thread {}",
            target.process_id, target.thread_id
        );
        send(agent, ACTIVATE, 0, 0)?;
        send(agent, SET_FOCUS, 0, 0)?;
        verify_desktop(baseline, automated)?;
        if hook.veto_counts().0 == 0 || hook.veto_counts().1 == 0 {
            return Err("CBT activation and focus vetoes were not exercised".into());
        }
        let timeout = Duration::from_secs(2);
        let stage = |step| -> Result<(), Box<dyn Error>> {
            send(human, preview::STATUS, step, 0)?;
            if owned && !automated {
                std::thread::sleep(Duration::from_millis(450));
            }
            Ok(())
        };
        stage(1)?;
        hook.text(agent_edit, "abc中😀", timeout)?;
        if text(agent_edit)? != "abc中😀" {
            return Err("native EDIT did not insert UTF-16 text".into());
        }
        hook.key(agent_edit, VK_HOME, KeyModifiers::default(), timeout)?;
        hook.key(agent_edit, VK_RIGHT, KeyModifiers::default(), timeout)?;
        hook.key(agent_edit, VK_DELETE, KeyModifiers::default(), timeout)?;
        if text(agent_edit)? != "ac中😀" {
            return Err("native EDIT did not apply Home/Right/Delete".into());
        }
        hook.key(agent_edit, VK_HOME, KeyModifiers::default(), timeout)?;
        hook.key(
            agent_edit,
            VK_RIGHT,
            KeyModifiers {
                shift: true,
                control: false,
            },
            timeout,
        )?;
        hook.text(agent_edit, "Z", timeout)?;
        if text(agent_edit)? != "Zc中😀" {
            return Err("Shift selection did not replace the selected character".into());
        }
        if hook.text(human_edit, "must not arrive", timeout).is_ok() {
            return Err("sibling human control accepted input".into());
        }
        if text(human_edit)? != "human baseline" || send(agent, QUERY, 5, 0)? != keyboard {
            return Err("human text or GUI-thread modifier state changed".into());
        }
        verify_desktop(baseline, automated)?;
        println!(
            "native EDIT text, surrogate pairs, navigation, deletion, Shift selection, sibling rejection and modifier restoration verified"
        );
        stage(2)?;
        hook.mouse(button, MouseAction::Move, Point { x: 30, y: 15 }, timeout)?;
        hook.mouse(
            button,
            MouseAction::ClickLeft,
            Point { x: 30, y: 15 },
            timeout,
        )?;
        if send(agent, QUERY, 4, 0)? != 1 {
            return Err("native BUTTON did not receive a message-backed click".into());
        }
        verify_desktop(baseline, automated)?;
        println!("native BUTTON message-backed click verified without cursor or focus changes");
        stage(3)?;
        let start = Point {
            x: send(agent, QUERY, 7, 0)? as i32,
            y: send(agent, QUERY, 8, 0)? as i32,
        };
        let left = send(agent, QUERY, 10, 0)? as i32;
        let right = send(agent, QUERY, 9, 0)? as i32;
        let end = left + (right - left) * 4 / 5;
        let path: Vec<Point> = (0..=24)
            .map(|step| Point {
                x: start.x + (end - start.x) * step / 24,
                y: start.y,
            })
            .collect();
        if hook.drag(slider, &path[..1], timeout).is_ok() {
            return Err("one-point drag was accepted".into());
        }
        hook.drag(slider, &path, timeout)?;
        let position = send(agent, QUERY, 6, 0)?;
        if !(75..=85).contains(&position) {
            return Err(format!("native slider did not drag to 80: {position}").into());
        }
        verify_desktop(baseline, automated)?;
        println!(
            "native slider dragged from 20 to {position}; user cursor/focus/capture preserved"
        );
        if native_touch {
            let mut pointer = TouchDevice::new()?;
            let mut point = POINT { x: 340, y: 220 };
            unsafe {
                windows_sys::Win32::Graphics::Gdi::ClientToScreen(agent as HWND, &mut point);
            }
            let point = Point {
                x: point.x,
                y: point.y,
            };
            let hit = unsafe {
                WindowFromPoint(POINT {
                    x: point.x,
                    y: point.y,
                })
            };
            if unsafe { GetAncestor(hit, GA_ROOT) } as usize != agent {
                return Err(format!("disposable touch target is occluded; refusing input: target={target:?} point={point:?} hit={hit:?}").into());
            }
            pointer.inject(ContactPhase::Down, point)?;
            std::thread::sleep(Duration::from_millis(60));
            pointer.inject(ContactPhase::Up, point)?;
            std::thread::sleep(Duration::from_millis(160));
            println!(
                "touch observations: down={} up={} invalid={} activations={} vetoes={:?}",
                send(agent, QUERY, 1, 0)?,
                send(agent, QUERY, 2, 0)?,
                send(agent, QUERY, 3, 0)?,
                send(agent, QUERY, 0, 0)?,
                hook.veto_counts()
            );
            verify_desktop(baseline, automated)?;
            if send(agent, QUERY, 1, 0)? != 1
                || send(agent, QUERY, 2, 0)? != 1
                || send(agent, QUERY, 3, 0)? != 0
            {
                return Err("default-handler touch was not delivered intact".into());
            }
            let mut point = POINT { x: 30, y: 15 };
            unsafe {
                windows_sys::Win32::Graphics::Gdi::ClientToScreen(button as HWND, &mut point);
            }
            if unsafe { WindowFromPoint(point) } != button as HWND {
                return Err("disposable button is occluded; refusing input".into());
            }
            pointer.inject(
                ContactPhase::Down,
                Point {
                    x: point.x,
                    y: point.y,
                },
            )?;
            std::thread::sleep(Duration::from_millis(60));
            pointer.inject(
                ContactPhase::Up,
                Point {
                    x: point.x,
                    y: point.y,
                },
            )?;
            std::thread::sleep(Duration::from_millis(160));
            verify_desktop(baseline, automated)?;
            if send(agent, QUERY, 4, 0)? != 2 {
                return Err("native BUTTON did not receive a touch click".into());
            }
        }
        send(human, CAPTURE, 1, 0)?;
        let captured = DesktopState::read()?;
        if captured.capture != human_edit {
            return Err("fixture did not establish human capture".into());
        }
        if hook
            .mouse(
                button,
                MouseAction::ClickLeft,
                Point { x: 30, y: 15 },
                timeout,
            )
            .is_ok()
        {
            return Err("mouse operation took over existing human capture".into());
        }
        verify_desktop(captured, automated)?;
        if send(agent, QUERY, 4, 0)? != if native_touch { 2 } else { 1 } {
            return Err("rejected mouse command still clicked the button".into());
        }
        send(human, CAPTURE, 0, 0)?;
        hook.close()?;
        let mut hook = WindowHook::attach(&target, &dll)?;
        send(human, CAPTURE, 1, 0)?;
        let captured = DesktopState::read()?;
        if hook.drag(slider, &path, timeout).is_ok() {
            return Err("drag stole existing human capture".into());
        }
        verify_desktop(captured, automated)?;
        if send(agent, QUERY, 6, 0)? != position {
            return Err("rejected drag moved the slider".into());
        }
        send(human, CAPTURE, 0, 0)?;
        hook.close()?;
        send(agent, ACTIVATE, 0, 0)?;
        if unsafe { GetForegroundWindow() } as usize != agent {
            return Err("hot detach did not restore normal activation".into());
        }
        send(human, ACTIVATE, 0, 0)?;
        let target = Window::inspect(agent)?;
        let mut hook = WindowHook::attach(&target, &dll)?;
        // Stall the target queue: timeout must cancel an unstarted effect, and the
        // eventual wake must not type after the caller has abandoned the operation.
        if unsafe { PostMessageW(agent as HWND, PAUSE, 350, 0) } == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        std::thread::sleep(Duration::from_millis(30));
        if hook
            .text(agent_edit, "late", Duration::from_millis(30))
            .is_ok()
        {
            return Err("hung queue did not time out".into());
        }
        if hook.text(agent_edit, "retry", timeout).is_ok() {
            return Err("uncertain hook accepted further input".into());
        }
        std::thread::sleep(Duration::from_millis(400));
        if text(agent_edit)? != "Zc中😀" {
            return Err("timed out command executed later".into());
        }
        hook.close()?;
        println!("hot detach/reattach and cancellation without delayed replay verified");
        let mut owner = Fixture(
            Command::new(std::env::current_exe()?)
                .args(["--lease-target", &agent.to_string(), "--dll"])
                .arg(&dll)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()?,
        );
        let stdout = owner.0.stdout.take().ok_or("missing lease owner stdout")?;
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut line = String::new();
            let result = BufReader::new(stdout).read_line(&mut line).map(|_| line);
            let _ = tx.send(result);
        });
        if rx.recv_timeout(Duration::from_secs(5))??.trim() != "LEASE READY" {
            return Err("lease owner did not attach".into());
        }
        send(agent, ACTIVATE, 0, 0)?;
        if unsafe { GetForegroundWindow() } as usize != human {
            return Err("lease owner did not veto activation".into());
        }
        owner.0.kill()?;
        owner.0.wait()?;
        send(agent, ACTIVATE, 0, 0)?;
        if unsafe { GetForegroundWindow() } as usize != agent {
            return Err("dead hook owner left activation blocked".into());
        }
        println!("owner process termination releases activation policy");
        send(human, ACTIVATE, 0, 0)?;
        send(human, preview::STATUS, 4, 0)?;
        if owned {
            std::thread::sleep(Duration::from_millis(150));
            if send(human, preview::STATS, 0, 0)? == 0
                || send(human, preview::STATS, 2, 0)? == 0
                || send(human, preview::STATS, 1, 0)? != 0
            {
                return Err("preview did not render live captured frames".into());
            }
        }
        Ok(())
    })();
    if let Err(error) = &verification {
        eprintln!("{error}");
        let _ = send(human, preview::STATUS, 5, 0);
    }
    if !automated && owned {
        while fixture.0.try_wait()?.is_none() {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    unsafe {
        PostMessageW(human as HWND, WM_CLOSE, 0, 0);
    }
    fixture.0.wait()?;
    if let Some(workspace) = workspace {
        workspace.close()?;
    }
    verification
}
