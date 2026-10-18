//! Startup placement is supplied before the target creates a window. Windows
//! applications can override it; callers must inspect the resulting windows.
use super::{DpiContext, displays, last_error};
use crate::{DisplayId, Error, Rect, Result, hook_protocol::native::process_started};
use std::{os::windows::ffi::OsStrExt, path::Path, ptr};
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE, HWND, RECT},
    Graphics::Gdi::{MONITOR_DEFAULTTONULL, MonitorFromRect},
    System::Threading::{
        CREATE_NO_WINDOW, CreateProcessW, GetProcessId, PROCESS_INFORMATION,
        STARTF_FORCEOFFFEEDBACK, STARTF_USEPOSITION, STARTF_USESHOWWINDOW, STARTF_USESIZE,
        STARTUPINFOW,
    },
    UI::{Shell::*, WindowsAndMessaging::*},
};

#[derive(Clone, Copy, Debug)]
pub struct LaunchedProcess {
    pub process_id: u32,
    pub process_started: u64,
}
struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
fn process(handle: HANDLE) -> Result<LaunchedProcess> {
    let pid = unsafe { GetProcessId(handle) };
    let started =
        process_started(handle).ok_or_else(|| last_error("read launched process identity"))?;
    if pid == 0 {
        return Err(last_error("read launched process ID"));
    }
    Ok(LaunchedProcess {
        process_id: pid,
        process_started: started,
    })
}

/// Invoke only in a credential-free launcher process. The target inherits this
/// process's environment for EXEs, never its handles. No existing window is repositioned.
/// Shell activation may return no process handle (e.g. a broker/existing instance).
pub fn launch_on_display(
    app: &str,
    display_id: &DisplayId,
    bounds: Rect,
) -> Result<Option<LaunchedProcess>> {
    let _dpi = DpiContext::enter()?;
    let display = displays()?
        .into_iter()
        .find(|d| &d.id == display_id)
        .ok_or(Error::InvalidState("launch display is unavailable"))?;
    if !display.indirect || display.cloned || !display.bounds.contains_rect(bounds) {
        return Err(Error::InvalidState(
            "launch placement must fit the virtual display",
        ));
    }
    if app.contains('\0') {
        return Err(Error::InvalidState("invalid application target"));
    }
    let path = Path::new(app);
    if !app.starts_with("shell:AppsFolder\\") {
        if !path.is_absolute() || !path.is_file() {
            return Err(Error::InvalidState(
                "application must be an existing absolute path",
            ));
        }
        let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let startup = STARTUPINFOW {
            cb: std::mem::size_of::<STARTUPINFOW>() as u32,
            dwFlags: STARTF_USEPOSITION
                | STARTF_USESIZE
                | STARTF_USESHOWWINDOW
                | STARTF_FORCEOFFFEEDBACK,
            dwX: bounds.left as u32,
            dwY: bounds.top as u32,
            dwXSize: bounds.width()? as u32,
            dwYSize: bounds.height()? as u32,
            wShowWindow: SW_SHOWNOACTIVATE as u16,
            ..Default::default()
        };
        let mut command_line: Vec<u16> = std::iter::once(34)
            .chain(path[..path.len() - 1].iter().copied())
            .chain([34, 0])
            .collect();
        let mut info = PROCESS_INFORMATION::default();
        if unsafe {
            CreateProcessW(
                path.as_ptr(),
                command_line.as_mut_ptr(),
                ptr::null(),
                ptr::null(),
                0,
                CREATE_NO_WINDOW,
                ptr::null(),
                ptr::null(),
                &startup,
                &mut info,
            )
        } == 0
        {
            return Err(last_error("launch application on virtual display"));
        }
        let process_handle = Handle(info.hProcess);
        let _thread = Handle(info.hThread);
        return process(process_handle.0).map(Some);
    }
    use ::windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
        .ok()
        .map_err(|source| Error::Automation {
            operation: "initialize launch Shell",
            source,
        })?;
    struct Com;
    impl Drop for Com {
        fn drop(&mut self) {
            unsafe {
                CoUninitialize();
            }
        }
    }
    let _com = Com;
    let rectangle = RECT {
        left: bounds.left,
        top: bounds.top,
        right: bounds.right,
        bottom: bounds.bottom,
    };
    let monitor = unsafe { MonitorFromRect(&rectangle, MONITOR_DEFAULTTONULL) };
    if monitor.is_null() {
        return Err(last_error("resolve launch monitor"));
    }
    // A hidden, nonactivating owner gives Shell a virtual-monitor context without
    // moving focus/cursor or making the virtual display the user's primary one.
    let anchor = unsafe {
        CreateWindowExW(
            WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
            windows_sys::core::w!("STATIC"),
            ptr::null(),
            WS_POPUP,
            bounds.left,
            bounds.top,
            bounds.width()?,
            bounds.height()?,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null(),
        )
    };
    if anchor.is_null() {
        return Err(last_error("create launch monitor anchor"));
    }
    struct Anchor(HWND);
    impl Drop for Anchor {
        fn drop(&mut self) {
            unsafe {
                DestroyWindow(self.0);
            }
        }
    }
    let _anchor = Anchor(anchor);
    let app: Vec<u16> = app.encode_utf16().chain(Some(0)).collect();
    let mut launch = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS
            | SEE_MASK_FLAG_NO_UI
            | SEE_MASK_HMONITOR
            | SEE_MASK_NOASYNC
            | SEE_MASK_NO_CONSOLE,
        hwnd: anchor,
        lpFile: app.as_ptr(),
        nShow: SW_SHOWNOACTIVATE,
        Anonymous: SHELLEXECUTEINFOW_0 { hMonitor: monitor },
        ..Default::default()
    };
    if unsafe { ShellExecuteExW(&mut launch) } == 0 {
        return Err(last_error("launch Shell app on virtual display"));
    }
    if launch.hProcess.is_null() {
        return Ok(None);
    }
    let handle = Handle(launch.hProcess);
    process(handle.0).map(Some)
}
