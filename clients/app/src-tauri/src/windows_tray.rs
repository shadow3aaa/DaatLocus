//! A native, keyboard-accessible tray menu. No second WebView or service controls.
use std::{mem::size_of, ptr};
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM},
    Graphics::{
        Dwm::{DWMWA_WINDOW_CORNER_PREFERENCE, DwmSetWindowAttribute},
        Gdi::*,
    },
    UI::{
        Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW},
        Controls::{DRAWITEMSTRUCT, MEASUREITEMSTRUCT, ODS_SELECTED, ODT_MENU},
        HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI},
        Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
        WindowsAndMessaging::*,
    },
};

const TOGGLE: u32 = 0xDAA1;
const QUIT: u32 = 0xDAA2;
const SUBCLASS: usize = 0xDAA7;

pub enum Action {
    Toggle,
    Quit,
}

struct Popup {
    menu: HMENU,
    font: HFONT,
    background: HBRUSH,
    text: u32,
    hover: u32,
    hover_text: u32,
    dpi: u32,
    labels: [Vec<u16>; 2],
}

impl Popup {
    fn px(&self, logical: i32) -> i32 {
        (logical * self.dpi as i32 + 48) / 96
    }

    fn new(position: POINT, toggle: &str, quit: &str, dark: bool) -> Option<Self> {
        let mut dpi = 96;
        let mut vertical = 96;
        // The tray may be on a different monitor from the conversation window.
        unsafe {
            if GetDpiForMonitor(
                MonitorFromPoint(position, MONITOR_DEFAULTTONEAREST),
                MDT_EFFECTIVE_DPI,
                &mut dpi,
                &mut vertical,
            ) < 0
            {
                dpi = 96;
            }
        }
        let mut contrast = HIGHCONTRASTW {
            cbSize: size_of::<HIGHCONTRASTW>() as u32,
            ..Default::default()
        };
        unsafe {
            SystemParametersInfoW(
                SPI_GETHIGHCONTRAST,
                contrast.cbSize,
                &mut contrast as *mut _ as _,
                0,
            );
        }
        let high_contrast = contrast.dwFlags & HCF_HIGHCONTRASTON != 0;
        let (background, text, hover) = if high_contrast {
            unsafe {
                (
                    GetSysColor(COLOR_MENU),
                    GetSysColor(COLOR_MENUTEXT),
                    GetSysColor(COLOR_HIGHLIGHT),
                )
            }
        } else if dark {
            (0x00212121, 0x00ededed, 0x00333333)
        } else {
            (0x00ffffff, 0x000d0d0d, 0x00f0f0f0)
        };
        let popup = Self {
            menu: unsafe { CreatePopupMenu() },
            font: unsafe {
                CreateFontW(
                    -(16 * dpi as i32 / 96),
                    0,
                    0,
                    0,
                    FW_NORMAL as i32,
                    0,
                    0,
                    0,
                    DEFAULT_CHARSET as u32,
                    0,
                    0,
                    CLEARTYPE_QUALITY as u32,
                    0,
                    windows_sys::w!("Segoe UI"),
                )
            },
            background: unsafe { CreateSolidBrush(background) },
            text,
            hover,
            hover_text: if high_contrast {
                unsafe { GetSysColor(COLOR_HIGHLIGHTTEXT) }
            } else {
                text
            },
            dpi,
            labels: [toggle, quit].map(|label| label.encode_utf16().chain(Some(0)).collect()),
        };
        if popup.menu.is_null() || popup.font.is_null() || popup.background.is_null() {
            return None;
        }
        let menu_info = MENUINFO {
            cbSize: size_of::<MENUINFO>() as u32,
            fMask: MIM_BACKGROUND | MIM_STYLE,
            dwStyle: MNS_NOCHECK,
            hbrBack: popup.background,
            ..Default::default()
        };
        unsafe { SetMenuInfo(popup.menu, &menu_info) };
        for (index, label) in popup.labels.iter().enumerate() {
            let info = MENUITEMINFOW {
                cbSize: size_of::<MENUITEMINFOW>() as u32,
                fMask: MIIM_ID | MIIM_FTYPE | MIIM_STRING,
                fType: MFT_OWNERDRAW,
                wID: [TOGGLE, QUIT][index],
                // Retain native menu strings for accessibility and type-ahead.
                dwTypeData: label.as_ptr() as _,
                cch: (label.len() - 1) as u32,
                ..Default::default()
            };
            if unsafe { InsertMenuItemW(popup.menu, index as u32, 1, &info) } == 0 {
                return None;
            }
        }
        Some(popup)
    }

    unsafe fn draw(&self, item: &DRAWITEMSTRUCT) {
        unsafe {
            let dc = item.hDC;
            FillRect(dc, &item.rcItem, self.background);
            let mut row = item.rcItem;
            row.left += self.px(4);
            row.right -= self.px(4);
            row.top += self.px(2);
            row.bottom -= self.px(2);
            if item.itemState & ODS_SELECTED != 0 {
                let brush = CreateSolidBrush(self.hover);
                let old_brush = SelectObject(dc, brush);
                let old_pen = SelectObject(dc, GetStockObject(NULL_PEN));
                RoundRect(
                    dc,
                    row.left,
                    row.top,
                    row.right,
                    row.bottom,
                    self.px(10),
                    self.px(10),
                );
                SelectObject(dc, old_pen);
                SelectObject(dc, old_brush);
                DeleteObject(brush);
            }
            let old_font = SelectObject(dc, self.font);
            let old_color = SetTextColor(
                dc,
                if item.itemState & ODS_SELECTED != 0 {
                    self.hover_text
                } else {
                    self.text
                },
            );
            let old_mode = SetBkMode(dc, TRANSPARENT as i32);
            let mut text = item.rcItem;
            text.left += self.px(18);
            text.right -= self.px(18);
            let label = &self.labels[usize::from(item.itemID == QUIT)];
            DrawTextW(
                dc,
                label.as_ptr(),
                (label.len() - 1) as i32,
                &mut text,
                DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX,
            );
            SetBkMode(dc, old_mode);
            SetTextColor(dc, old_color);
            SelectObject(dc, old_font);

            // Keep the system popup's shadow and positioning; opt into Windows
            // 11's rounded frame rather than drawing a fake browser menu.
            let window = WindowFromDC(dc);
            let mut class = [0u16; 16];
            let length = GetClassNameW(window, class.as_mut_ptr(), class.len() as i32);
            if length == 6 && class[..6] == [35, 51, 50, 55, 54, 56] {
                let corner = 2u32; // DWMWCP_ROUND
                DwmSetWindowAttribute(
                    window,
                    DWMWA_WINDOW_CORNER_PREFERENCE as u32,
                    &corner as *const _ as _,
                    size_of::<u32>() as u32,
                );
            }
        }
    }
}

impl Drop for Popup {
    fn drop(&mut self) {
        unsafe {
            DestroyMenu(self.menu);
            DeleteObject(self.font);
            DeleteObject(self.background);
        }
    }
}

unsafe extern "system" fn menu_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _: usize,
    data: usize,
) -> LRESULT {
    // Installed only for the synchronous TrackPopupMenuEx call below. The
    // popup and all GDI resources outlive every callback and are then released.
    let popup = unsafe { &*(data as *const Popup) };
    unsafe {
        if message == WM_MEASUREITEM && lparam != 0 {
            let item = &mut *(lparam as *mut MEASUREITEMSTRUCT);
            if item.CtlType == ODT_MENU && matches!(item.itemID, TOGGLE | QUIT) {
                item.itemWidth = popup.px(216) as u32;
                item.itemHeight = popup.px(48) as u32;
                return 1;
            }
        }
        if message == WM_DRAWITEM && lparam != 0 {
            let item = &*(lparam as *const DRAWITEMSTRUCT);
            if item.CtlType == ODT_MENU && item.hwndItem == popup.menu {
                popup.draw(item);
                return 1;
            }
        }
        DefSubclassProc(hwnd, message, wparam, lparam)
    }
}

/// Must run on the owner's native UI thread. Windows handles arrows, Enter,
/// Escape, outside clicks, monitor bounds, and screen-reader menu navigation.
pub fn show(owner: HWND, position: POINT, toggle: &str, quit: &str, dark: bool) -> Option<Action> {
    let popup = Popup::new(position, toggle, quit, dark)?;
    unsafe {
        if SetWindowSubclass(
            owner,
            Some(menu_proc),
            SUBCLASS,
            &popup as *const _ as usize,
        ) == 0
        {
            return None;
        }
        SetForegroundWindow(owner);
        let choice = TrackPopupMenuEx(
            popup.menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_RIGHTALIGN | TPM_BOTTOMALIGN,
            position.x,
            position.y,
            owner,
            ptr::null(),
        );
        RemoveWindowSubclass(owner, Some(menu_proc), SUBCLASS);
        PostMessageW(owner, WM_NULL, 0, 0);
        match choice as u32 {
            TOGGLE => Some(Action::Toggle),
            QUIT => Some(Action::Quit),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "activate test window; Escape, Down+Enter, Down+Down+Enter, Escape"]
    fn native_popup_keyboard_and_dismissal() {
        // Exercise the production HMENU/drawing/subclass integration without
        // starting a service, accessing an account, or quitting a real client.
        unsafe {
            let owner = CreateWindowExW(
                0,
                windows_sys::w!("STATIC"),
                windows_sys::w!("Daat Locus tray check · Escape"),
                WS_OVERLAPPEDWINDOW,
                200,
                200,
                420,
                300,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null(),
            );
            assert!(!owner.is_null());
            struct Owner(HWND);
            impl Drop for Owner {
                fn drop(&mut self) {
                    unsafe {
                        DestroyWindow(self.0);
                    }
                }
            }
            let _owner = Owner(owner);
            ShowWindow(owner, SW_SHOWNOACTIVATE);
            while GetForegroundWindow() != owner {
                let mut message = MSG::default();
                assert!(GetMessageW(&mut message, ptr::null_mut(), 0, 0) > 0);
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
            let point = POINT { x: 580, y: 440 };
            assert!(show(owner, point, "最小化到托盘", "退出 Daat Locus", false).is_none());
            SetWindowTextW(owner, windows_sys::w!("Daat Locus tray check · Open"));
            assert!(matches!(
                show(owner, point, "打开 Daat Locus", "退出 Daat Locus", false),
                Some(Action::Toggle)
            ));
            SetWindowTextW(owner, windows_sys::w!("Daat Locus tray check · Quit"));
            assert!(matches!(
                show(owner, point, "最小化到托盘", "退出 Daat Locus", false),
                Some(Action::Quit)
            ));
            SetWindowTextW(owner, windows_sys::w!("Daat Locus tray check · Dark"));
            assert!(show(owner, point, "最小化到托盘", "退出 Daat Locus", true).is_none());
        }
    }
}
