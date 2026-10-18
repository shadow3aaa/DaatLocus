//! Runs only on the selected GUI thread. No model/runtime dependencies or I/O logs.
#![cfg(windows)]

#[allow(dead_code)]
#[path = "../../src/hook_protocol.rs"]
mod protocol;

use protocol::{Command, Shared, native::*};
use std::{ptr, sync::atomic::Ordering};
use windows_sys::Win32::{
    Foundation::{
        ERROR_BUSY, ERROR_INVALID_PARAMETER, ERROR_INVALID_WINDOW_HANDLE, GetLastError, HWND,
        LPARAM, LRESULT, POINT, RECT, WPARAM,
    },
    Graphics::Gdi::ClientToScreen,
    System::Threading::{GetCurrentProcess, GetCurrentProcessId, GetCurrentThreadId},
    UI::{Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};

#[unsafe(no_mangle)]
pub extern "system" fn DlWorkspaceHookAbi() -> u32 {
    protocol::ABI
}

fn mapping() -> Option<Mapping> {
    let started = process_started(unsafe { GetCurrentProcess() })?;
    let name = protocol::mapping_name(
        unsafe { GetCurrentProcessId() },
        unsafe { GetCurrentThreadId() },
        started,
    );
    let mapping = Mapping::open(&name)?;
    let shared = mapping.shared();
    if !shared.valid()
        || shared.target_pid != unsafe { GetCurrentProcessId() }
        || shared.target_started != started
        || shared.enabled.load(Ordering::Acquire) == 0
        || !owner_alive(shared)
        || !frame_alive(shared)
    {
        return None;
    }
    Some(mapping)
}

fn frame_alive(shared: &Shared) -> bool {
    let mut pid = 0;
    let thread = unsafe { GetWindowThreadProcessId(shared.root as HWND, &mut pid) };
    if thread != shared.frame_thread || pid != shared.frame_pid {
        return false;
    }
    let process = unsafe {
        windows_sys::Win32::System::Threading::OpenProcess(
            windows_sys::Win32::System::Threading::PROCESS_QUERY_LIMITED_INFORMATION,
            0,
            pid,
        )
    };
    if process.is_null() {
        return false;
    }
    let alive = process_started(process) == Some(shared.frame_started);
    unsafe {
        windows_sys::Win32::Foundation::CloseHandle(process);
    }
    alive
}

fn belongs_to_target(shared: &Shared, hwnd: HWND) -> bool {
    let mut pid = 0;
    let same_thread = !hwnd.is_null()
        && unsafe { GetWindowThreadProcessId(hwnd, &mut pid) } == unsafe { GetCurrentThreadId() }
        && pid == shared.target_pid;
    if !same_thread {
        return false;
    }
    belongs_to_family(shared, hwnd)
}

fn belongs_to_family(shared: &Shared, hwnd: HWND) -> bool {
    if hwnd.is_null() {
        return false;
    }
    let mut root = unsafe { GetAncestor(hwnd, GA_ROOT) };
    for _ in 0..64 {
        if root as u64 == shared.root {
            return true;
        }
        root = unsafe { GetWindow(root, GW_OWNER) };
        if root.is_null() {
            break;
        }
    }
    false
}

fn in_target(shared: &Shared, hwnd: HWND) -> bool {
    shared.enabled.load(Ordering::Acquire) != 0 && belongs_to_target(shared, hwnd)
}

fn capture_available() -> bool {
    let foreground = unsafe { GetForegroundWindow() };
    let thread = unsafe { GetWindowThreadProcessId(foreground, ptr::null_mut()) };
    let mut gui = GUITHREADINFO {
        cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    thread != 0
        && unsafe { GetGUIThreadInfo(thread, &mut gui) } != 0
        && gui.hwndCapture.is_null()
        && unsafe { GetCapture() }.is_null()
}

struct Gesture<'a> {
    shared: &'a Shared,
    target: HWND,
    released: bool,
}
impl Drop for Gesture<'_> {
    fn drop(&mut self) {
        // Cancellation clears the control's pressed/dragging state as well as capture.
        if !self.released && belongs_to_target(self.shared, self.target) {
            unsafe {
                SendMessageW(self.target, WM_CANCELMODE, 0, 0);
            }
        }
        let capture = unsafe { GetCapture() };
        if !capture.is_null() && belongs_to_target(self.shared, capture) {
            unsafe {
                ReleaseCapture();
            }
        }
    }
}

fn packed(point: protocol::DragPoint) -> isize {
    (point.x as u16 as u32 | ((point.y as u16 as u32) << 16)) as isize
}

/// # Safety
/// Called by Windows with the WH_CBT callback contract.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn DlWorkspaceCbt(code: i32, w: WPARAM, l: LPARAM) -> LRESULT {
    if code >= 0
        && let Some(mapping) = mapping()
    {
        let shared = mapping.shared();
        if code == HCBT_DESTROYWND as i32
            && (w as u64 == shared.root || w as u64 == shared.transport)
        {
            shared.enabled.store(0, Ordering::Release);
            shared.cancel_pending();
        // CBT's requested activation/focus window can belong to another GUI
        // thread/process in this visual family. Input dispatch still requires
        // the executing thread; vetoing activation must follow real ancestry.
        } else if shared.enabled.load(Ordering::Acquire) != 0
            && belongs_to_family(shared, w as HWND)
        {
            if code == HCBT_ACTIVATE as i32 {
                shared.activation_vetoes.fetch_add(1, Ordering::Relaxed);
                return 1;
            }
            if code == HCBT_SETFOCUS as i32 {
                shared.focus_vetoes.fetch_add(1, Ordering::Relaxed);
                return 1;
            }
        }
    }
    unsafe { CallNextHookEx(ptr::null_mut(), code, w, l) }
}

struct KeyboardState([u8; 256]);
impl Drop for KeyboardState {
    fn drop(&mut self) {
        unsafe {
            SetKeyboardState(self.0.as_ptr());
        }
    }
}

fn execute(shared: &Shared, command: Command) -> (u32, u32) {
    if command.kind == protocol::PING {
        return (0, 0);
    }
    let hwnd = command.target as HWND;
    if !in_target(shared, hwnd) || unsafe { IsWindowEnabled(hwnd) } == 0 {
        return (ERROR_INVALID_WINDOW_HANDLE, 0);
    }
    match command.kind {
        protocol::CLOSE => {
            if unsafe { GetAncestor(hwnd, GA_ROOT) } != hwnd {
                return (ERROR_INVALID_PARAMETER, 0);
            }
            unsafe {
                SendMessageW(hwnd, WM_CLOSE, 0, 0);
            }
            (0, 1)
        }
        protocol::TEXT => {
            if command.length as usize > protocol::MAX_TEXT {
                return (ERROR_INVALID_PARAMETER, 0);
            }
            for (index, unit) in command.text[..command.length as usize].iter().enumerate() {
                if !in_target(shared, hwnd) {
                    return (ERROR_INVALID_WINDOW_HANDLE, index as u32);
                }
                unsafe {
                    SendMessageW(hwnd, WM_CHAR, *unit as usize, 1);
                }
            }
            (0, command.length)
        }
        protocol::KEY => {
            if command.key == 0 || command.key > 254 || command.modifiers & !3 != 0 {
                return (ERROR_INVALID_PARAMETER, 0);
            }
            let mut original = [0; 256];
            if unsafe { GetKeyboardState(original.as_mut_ptr()) } == 0 {
                return (unsafe { GetLastError() }, 0);
            }
            let old = KeyboardState(original);
            // Guard restores the complete GUI-thread keyboard state even after reentrant
            // window destruction. This does not virtualize GetAsyncKeyState or focus.
            let mut state = old.0;
            for key in &mut state {
                *key &= 1;
            }
            if command.modifiers & 1 != 0 {
                state[VK_SHIFT as usize] = 0x80;
                state[VK_LSHIFT as usize] = 0x80;
            }
            if command.modifiers & 2 != 0 {
                state[VK_CONTROL as usize] = 0x80;
                state[VK_LCONTROL as usize] = 0x80;
            }
            state[command.key as usize] = 0x80;
            if unsafe { SetKeyboardState(state.as_ptr()) } == 0 {
                return (unsafe { GetLastError() }, 0);
            }
            let scan = unsafe { MapVirtualKeyW(command.key, MAPVK_VK_TO_VSC_EX) };
            let extended = if scan & 0xff00 != 0 { 1u32 << 24 } else { 0 };
            let parameters = 1 | ((scan & 0xff) << 16) | extended;
            unsafe {
                SendMessageW(hwnd, WM_KEYDOWN, command.key as usize, parameters as isize);
            }
            state[command.key as usize] = 0;
            if unsafe { SetKeyboardState(state.as_ptr()) } == 0 {
                return (unsafe { GetLastError() }, 1);
            }
            if !in_target(shared, hwnd) {
                return (ERROR_INVALID_WINDOW_HANDLE, 1);
            }
            unsafe {
                SendMessageW(
                    hwnd,
                    WM_KEYUP,
                    command.key as usize,
                    (parameters | 0xc0000000) as isize,
                );
            }
            if unsafe { SetKeyboardState(old.0.as_ptr()) } == 0 {
                return (unsafe { GetLastError() }, 2);
            }
            (0, 2)
        }
        protocol::MOUSE => {
            let mut rect = RECT::default();
            if unsafe { GetClientRect(hwnd, &mut rect) } == 0
                || command.x < 0
                || command.y < 0
                || command.x >= rect.right
                || command.y >= rect.bottom
                || command.x > i16::MAX as i32
                || command.y > i16::MAX as i32
            {
                return (ERROR_INVALID_PARAMETER, 0);
            }
            // Never take over an existing drag/capture, including on the same GUI thread.
            if !capture_available() {
                return (ERROR_BUSY, 0);
            }
            let mut gesture = Gesture {
                shared,
                target: hwnd,
                released: false,
            };
            let packed = (command.x as u16 as u32 | ((command.y as u16 as u32) << 16)) as isize;
            let count = match command.key {
                0 => {
                    unsafe {
                        SendMessageW(hwnd, WM_MOUSEMOVE, 0, packed);
                    }
                    1
                }
                1 => {
                    unsafe {
                        SendMessageW(hwnd, WM_MOUSEMOVE, 0, packed);
                    }
                    if !in_target(shared, hwnd) {
                        return (ERROR_INVALID_WINDOW_HANDLE, 1);
                    }
                    unsafe {
                        SendMessageW(hwnd, WM_LBUTTONDOWN, 1, packed);
                    }
                    if !in_target(shared, hwnd) {
                        return (ERROR_INVALID_WINDOW_HANDLE, 2);
                    }
                    unsafe {
                        SendMessageW(hwnd, WM_LBUTTONUP, 0, packed);
                    }
                    3
                }
                2 => {
                    let mut screen = POINT {
                        x: command.x,
                        y: command.y,
                    };
                    if unsafe { ClientToScreen(hwnd, &mut screen) } == 0 {
                        return (unsafe { GetLastError() }, 0);
                    }
                    let position =
                        (screen.x as u16 as u32 | ((screen.y as u16 as u32) << 16)) as isize;
                    unsafe {
                        SendMessageW(
                            hwnd,
                            WM_MOUSEWHEEL,
                            (command.modifiers as u16 as usize) << 16,
                            position,
                        );
                    }
                    1
                }
                _ => return (ERROR_INVALID_PARAMETER, 0),
            };
            gesture.released = true;
            (0, count)
        }
        protocol::DRAG => {
            let length = command.length as usize;
            if !(2..=protocol::MAX_DRAG_POINTS).contains(&length) {
                return (ERROR_INVALID_PARAMETER, 0);
            }
            let path = &command.path[..length];
            let mut rect = RECT::default();
            if path
                .iter()
                .any(|p| i16::try_from(p.x).is_err() || i16::try_from(p.y).is_err())
                || unsafe { GetClientRect(hwnd, &mut rect) } == 0
                || path[0].x < 0
                || path[0].y < 0
                || path[0].x >= rect.right
                || path[0].y >= rect.bottom
            {
                return (ERROR_INVALID_PARAMETER, 0);
            }
            if !capture_available() {
                return (ERROR_BUSY, 0);
            }
            let mut gesture = Gesture {
                shared,
                target: hwnd,
                released: false,
            };
            unsafe {
                SendMessageW(hwnd, WM_MOUSEMOVE, 0, packed(path[0]));
            }
            if !in_target(shared, hwnd) {
                return (ERROR_INVALID_WINDOW_HANDLE, 1);
            }
            unsafe {
                SendMessageW(hwnd, WM_LBUTTONDOWN, 1, packed(path[0]));
            }
            let mut completed = 2;
            for point in &path[1..] {
                if !in_target(shared, hwnd) {
                    return (ERROR_INVALID_WINDOW_HANDLE, completed);
                }
                unsafe {
                    SendMessageW(hwnd, WM_MOUSEMOVE, 1, packed(*point));
                }
                completed += 1;
            }
            if !in_target(shared, hwnd) {
                return (ERROR_INVALID_WINDOW_HANDLE, completed);
            }
            unsafe {
                SendMessageW(hwnd, WM_LBUTTONUP, 0, packed(path[length - 1]));
            }
            gesture.released = true;
            (0, completed + 1)
        }
        _ => (ERROR_INVALID_PARAMETER, 0),
    }
}

/// # Safety
/// Called by Windows with a valid MSG pointer under the WH_GETMESSAGE contract.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn DlWorkspaceGetMessage(code: i32, w: WPARAM, l: LPARAM) -> LRESULT {
    if code >= 0
        && w == PM_REMOVE as usize
        && let Some(mapping) = mapping()
    {
        let message = unsafe { &mut *(l as *mut MSG) };
        let name: Vec<u16> = protocol::MESSAGE.encode_utf16().chain(Some(0)).collect();
        let id = unsafe { RegisterWindowMessageW(name.as_ptr()) };
        let shared = mapping.shared();
        if id != 0
            && message.message == id
            && message.hwnd as u64 == shared.transport
            && message.wParam == shared.token as usize
        {
            // Consume our transport message; no raw pointers or command data in MSG.
            message.message = WM_NULL;
            message.wParam = 0;
            message.lParam = 0;
            if shared.claim() {
                // PENDING -> RUNNING transfers immutable command ownership to this thread.
                let command = unsafe { *shared.command.get() };
                let (error, completed) = execute(shared, command);
                shared.error.store(error, Ordering::Relaxed);
                shared.completed.store(completed, Ordering::Relaxed);
                shared.state.store(protocol::DONE, Ordering::Release);
            }
        }
    }
    unsafe { CallNextHookEx(ptr::null_mut(), code, w, l) }
}
