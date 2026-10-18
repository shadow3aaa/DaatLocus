//! Live IDD pixels in the disposable human window. Native loop stays on its GUI thread.
use daat_locus_virtual_workspace::{Frame, VirtualWorkspace, displays};
use std::{cell::RefCell, error::Error, ptr};
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM},
    Graphics::Gdi::*,
    UI::{Input::KeyboardAndMouse::EnableWindow, WindowsAndMessaging::*},
};

thread_local! { static VIEW: RefCell<Option<Viewer>> = const { RefCell::new(None) }; }
struct Viewer {
    workspace: VirtualWorkspace,
    frame: Option<Frame>,
    edit: HWND,
    start: HWND,
    status: &'static str,
    failed: bool,
    frames: u32,
    painted_frames: u32,
}
pub const STATUS: u32 = WM_APP + 46;
pub const STATS: u32 = WM_APP + 47;

pub fn attach(hwnd: HWND, edit: HWND, display: &str) -> Result<(), Box<dyn Error>> {
    let display = displays()?
        .into_iter()
        .find(|d| d.device_name == display)
        .ok_or("preview display disappeared")?;
    let workspace = VirtualWorkspace::attach(&display.id)?;
    let start = unsafe {
        CreateWindowExW(
            0,
            super::wide("BUTTON").as_ptr(),
            super::wide("Run tests").as_ptr(),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP,
            16,
            16,
            120,
            32,
            hwnd,
            3usize as _,
            ptr::null_mut(),
            ptr::null(),
        )
    };
    if start.is_null() {
        return Err(std::io::Error::last_os_error().into());
    }
    VIEW.with_borrow_mut(|view| {
        *view = Some(Viewer {
            workspace,
            frame: None,
            edit,
            start,
            status: "Ready. Run the tests to see text, clicks and dragging on the virtual screen.",
            failed: false,
            frames: 0,
            painted_frames: 0,
        })
    });
    if unsafe { SetTimer(hwnd, 1, 50, None) } == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    unsafe {
        SendMessageW(hwnd, WM_SIZE, 0, 0);
    }
    Ok(())
}

pub fn handle(hwnd: HWND, message: u32, w: WPARAM, _l: LPARAM) -> Option<LRESULT> {
    VIEW.with(|view| {
        let Ok(mut view) = view.try_borrow_mut() else { return None; };
        let view = view.as_mut()?;
        match message {
            WM_SIZE => {
                let mut rect = RECT::default();
                unsafe {
                    GetClientRect(hwnd, &mut rect);
                    MoveWindow(view.edit, 155, 16, (rect.right - 175).max(40), 32, 1);
                }
                Some(0)
            }
            WM_TIMER if w == 1 => {
                match view.workspace.capture() {
                    Ok(frame) => { view.frame = Some(frame); view.frames += 1; }
                    Err(_) => { view.failed = true; view.status = "Virtual display capture failed. Close this window and restart the lab."; }
                }
                unsafe { InvalidateRect(hwnd, ptr::null(), 0); }
                Some(0)
            }
            STATUS => {
                view.status = match w { 1 => "Testing background text, Unicode and keyboard navigation...", 2 => "Testing background button clicks...", 3 => "Testing a native slider drag...", 4 => "Passed: text, click, drag, capture protection and hook lifecycle.", 5 => "Test failed. See the terminal output for details.", _ => view.status };
                if w == 5 { view.failed = true; }
                let label = super::wide(match w { 4 => "Passed", 5 => "Failed", _ => "Testing..." });
                unsafe { SetWindowTextW(view.start, label.as_ptr()); EnableWindow(view.start, 0); InvalidateRect(hwnd, ptr::null(), 0); }
                Some(0)
            }
            STATS => Some(match w { 0 => view.frames as isize, 2 => view.painted_frames as isize, _ => view.failed as isize }),
            WM_PAINT => {
                let mut paint = PAINTSTRUCT::default();
                let dc = unsafe { BeginPaint(hwnd, &mut paint) };
                let mut bounds = RECT::default();
                unsafe { GetClientRect(hwnd, &mut bounds); FillRect(dc, &bounds, GetSysColorBrush(COLOR_WINDOW)); SetBkMode(dc, TRANSPARENT as i32); }
                let mut status = RECT { left: 16, top: 60, right: bounds.right - 16, bottom: 94 };
                let text = super::wide(view.status);
                unsafe { DrawTextW(dc, text.as_ptr(), -1, &mut status, DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS); }
                if let Some(frame) = &view.frame {
                    let available_width = (bounds.right - 32).max(1);
                    let available_height = (bounds.bottom - 120).max(1);
                    let scale = (available_width as f64 / frame.width as f64).min(available_height as f64 / frame.height as f64);
                    let width = (frame.width as f64 * scale).max(1.0) as i32;
                    let height = (frame.height as f64 * scale).max(1.0) as i32;
                    let left = 16 + (available_width - width) / 2;
                    let top = 104 + (available_height - height) / 2;
                    let info = BITMAPINFO { bmiHeader: BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32, biWidth: frame.width as i32, biHeight: -(frame.height as i32), biPlanes: 1, biBitCount: 32, biCompression: BI_RGB, ..Default::default() }, ..Default::default() };
                    let painted = unsafe { SetStretchBltMode(dc, COLORONCOLOR); StretchDIBits(dc, left, top, width, height, 0, 0, frame.width as i32, frame.height as i32, frame.bgra.as_ptr().cast(), &info, DIB_RGB_COLORS, SRCCOPY) };
                    if painted != 0 && painted != GDI_ERROR { view.painted_frames += 1; }
                    else { view.failed = true; view.status = "Drawing the virtual display failed."; }
                }
                unsafe { EndPaint(hwnd, &paint); }
                Some(0)
            }
            _ => None,
        }
    })
}

pub fn detach(hwnd: HWND) {
    unsafe {
        KillTimer(hwnd, 1);
    }
    VIEW.with_borrow_mut(|view| {
        view.take();
    });
}
