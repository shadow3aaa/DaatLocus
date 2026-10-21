//! One-shot compositor capture of an HWND. No desktop crop, retained session or idle frame pump.
use super::{DpiContext, Frame, Window};
use crate::{Error, Rect, Result};
use std::{
    sync::OnceLock,
    time::{Duration, Instant},
};
use windows::{
    Graphics::{
        Capture::{
            Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem,
            GraphicsCaptureSession,
        },
        DirectX::{Direct3D11::IDirect3DDevice, DirectXPixelFormat},
    },
    Win32::{
        Foundation::{HWND, RPC_E_CHANGED_MODE},
        Graphics::{
            Direct3D::D3D_DRIVER_TYPE_HARDWARE,
            Direct3D11::*,
            Dxgi::{DXGI_ERROR_WAS_STILL_DRAWING, IDXGIDevice},
        },
        System::WinRT::{
            Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess},
            Graphics::Capture::IGraphicsCaptureItemInterop,
            RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize,
        },
    },
    core::{Interface, factory},
};

struct Apartment(bool);
impl Drop for Apartment {
    fn drop(&mut self) {
        if self.0 {
            unsafe { RoUninitialize() };
        }
    }
}

fn retain_capture_module() -> windows::core::Result<()> {
    use windows_sys::Win32::{
        Foundation::FreeLibrary,
        System::LibraryLoader::{
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_PIN,
            GetModuleHandleExW, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
        },
    };

    static RESIDENT: OnceLock<windows::core::Result<()>> = OnceLock::new();
    RESIDENT
        .get_or_init(|| unsafe {
            // WGC can dispatch internal callbacks after Close and RoUninitialize.
            // Keep their code mapped for the process lifetime, before activating
            // any WGC object. A delay or a per-session library reference still
            // races those callbacks. Capture/GPU objects remain per observation.
            // https://github.com/robmikh/Win32CaptureSample/issues/99
            let library = LoadLibraryExW(
                windows_sys::w!("GraphicsCapture.dll"),
                std::ptr::null_mut(),
                LOAD_LIBRARY_SEARCH_SYSTEM32,
            );
            if library.is_null() {
                return Err(windows::core::Error::from_thread());
            }
            let mut pinned = std::ptr::null_mut();
            let result = if GetModuleHandleExW(
                GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_PIN,
                library.cast(),
                &mut pinned,
            ) == 0
            {
                Err(windows::core::Error::from_thread())
            } else {
                Ok(())
            };
            // Balance our temporary load. The loader's PIN survives this and
            // every subsequent COM library sweep, including other callers'.
            FreeLibrary(library);
            result
        })
        .clone()
}

struct Pool(Direct3D11CaptureFramePool);
impl Drop for Pool {
    fn drop(&mut self) {
        let _ = self.0.Close();
    }
}
struct Session(GraphicsCaptureSession);
impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.0.Close();
    }
}
struct Captured(Direct3D11CaptureFrame);
impl Drop for Captured {
    fn drop(&mut self) {
        let _ = self.0.Close();
    }
}

impl Window {
    /// Capture this window's compositor surface even when another window covers it.
    /// Coordinates retain GetWindowRect's physical origin; invisible borders are
    /// padded, never filled with pixels from the desktop. Geometry changes fail.
    /// Minimized, protected and capture-excluded windows are not guaranteed images.
    pub fn capture(&self) -> Result<Frame> {
        let _dpi = DpiContext::enter()?;
        self.validate()?;
        retain_capture_module().map_err(Error::Capture)?;
        let initialized = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
        if let Err(error) = &initialized
            && error.code() != RPC_E_CHANGED_MODE
        {
            return Err(Error::Capture(error.clone()));
        }
        let _apartment = Apartment(initialized.is_ok());
        capture(self).map_err(Error::Capture)
    }
}

fn capture(window: &Window) -> windows::core::Result<Frame> {
    let invalid = || {
        windows::core::Error::new(
            windows::Win32::Foundation::E_FAIL,
            "window capture geometry changed or is unsupported",
        )
    };
    if !GraphicsCaptureSession::IsSupported()? {
        return Err(invalid());
    }
    let interop = factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
    let item: GraphicsCaptureItem = unsafe { interop.CreateForWindow(HWND(window.handle as _))? };
    let size = item.Size()?;
    let surface_bounds = surface_bounds(window, size.Width, size.Height).map_err(|_| invalid())?;
    let (mut device, mut context) = (None, None);
    unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            Default::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )?;
    }
    let device = device.ok_or_else(invalid)?;
    let context = context.ok_or_else(invalid)?;
    let dxgi: IDXGIDevice = device.cast()?;
    let direct: IDirect3DDevice = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi)? }.cast()?;
    let pool = Pool(Direct3D11CaptureFramePool::CreateFreeThreaded(
        &direct,
        DirectXPixelFormat::B8G8R8A8UIntNormalized,
        1,
        size,
    )?);
    let session = Session(pool.0.CreateCaptureSession(&item)?);
    session.0.SetIsCursorCaptureEnabled(false)?;
    // Keep the OS capture indicator. Disabling it can require extra user consent.
    session.0.StartCapture()?;
    let deadline = Instant::now() + Duration::from_secs(3);
    let frame = loop {
        // The ABI legitimately returns S_OK + null while waiting. The generated
        // nonnullable projection turns that into an Error with an S_OK code.
        let mut raw = std::ptr::null_mut();
        unsafe {
            (Interface::vtable(&pool.0).TryGetNextFrame)(Interface::as_raw(&pool.0), &mut raw)
                .ok()?;
        }
        if !raw.is_null() {
            break Captured(unsafe { Direct3D11CaptureFrame::from_raw(raw) });
        }
        if Instant::now() >= deadline {
            return Err(windows::core::Error::new(
                windows::Win32::Foundation::E_FAIL,
                "timed out waiting for a window capture frame",
            ));
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    if frame.0.ContentSize()? != size {
        return Err(invalid());
    }
    let access: IDirect3DDxgiInterfaceAccess = frame.0.Surface()?.cast()?;
    let texture: ID3D11Texture2D = unsafe { access.GetInterface()? };
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    unsafe { texture.GetDesc(&mut desc) };
    if size.Width <= 0
        || size.Height <= 0
        || desc.Width < size.Width as u32
        || desc.Height < size.Height as u32
    {
        return Err(invalid());
    }
    desc.Usage = D3D11_USAGE_STAGING;
    desc.BindFlags = 0;
    desc.MiscFlags = 0;
    desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
    let mut staging = None;
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut staging))? };
    let staging = staging.ok_or_else(invalid)?;
    unsafe { context.CopyResource(&staging, &texture) };
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    loop {
        match unsafe {
            context.Map(
                &staging,
                0,
                D3D11_MAP_READ,
                D3D11_MAP_FLAG_DO_NOT_WAIT.0 as u32,
                Some(&mut mapped),
            )
        } {
            Ok(()) => break,
            Err(error)
                if error.code() == DXGI_ERROR_WAS_STILL_DRAWING && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) => return Err(error),
        }
    }
    // No fallible operation/panic between Map and Unmap may retain the resource.
    let pixels = copy_surface(
        window.bounds,
        surface_bounds,
        mapped.RowPitch as usize,
        mapped.pData.cast(),
    );
    unsafe { context.Unmap(&staging, 0) };
    let pixels = pixels.map_err(|_| invalid())?;
    window.validate().map_err(|_| invalid())?;
    if surface_bounds
        != self::surface_bounds(window, size.Width, size.Height).map_err(|_| invalid())?
    {
        return Err(invalid());
    }
    Ok(pixels)
}

fn surface_bounds(window: &Window, width: i32, height: i32) -> Result<Rect> {
    use windows_sys::Win32::{
        Foundation::RECT,
        Graphics::Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute},
    };
    let mut bounds = RECT::default();
    let status = unsafe {
        DwmGetWindowAttribute(
            window.handle as _,
            DWMWA_EXTENDED_FRAME_BOUNDS as u32,
            (&mut bounds as *mut RECT).cast(),
            std::mem::size_of::<RECT>() as u32,
        )
    };
    let extended = Rect {
        left: bounds.left,
        top: bounds.top,
        right: bounds.right,
        bottom: bounds.bottom,
    };
    if status >= 0
        && window.bounds.contains_rect(extended)
        && extended.width().ok() == Some(width)
        && extended.height().ok() == Some(height)
    {
        return Ok(extended);
    }
    if window.bounds.width()? == width && window.bounds.height()? == height {
        return Ok(window.bounds);
    }
    Err(Error::InvalidState(
        "compositor surface does not match the observed window bounds",
    ))
}

// The mapped texture owns at least height rows with RowPitch bytes each.
fn copy_surface(bounds: Rect, surface: Rect, pitch: usize, data: *const u8) -> Result<Frame> {
    let (width, height) = (bounds.width()? as usize, bounds.height()? as usize);
    let row = surface.width()? as usize * 4;
    if data.is_null() || !bounds.contains_rect(surface) || pitch < row {
        return Err(Error::InvalidState("invalid compositor texture"));
    }
    let length = width
        .checked_mul(height)
        .and_then(|n| n.checked_mul(4))
        .ok_or(Error::InvalidState("capture size overflow"))?;
    let mut bgra = Vec::new();
    bgra.try_reserve_exact(length)
        .map_err(|_| Error::InvalidState("cannot allocate capture buffer"))?;
    bgra.resize(length, 0);
    let x = (surface.left - bounds.left) as usize;
    let y = (surface.top - bounds.top) as usize;
    for at in 0..surface.height()? as usize {
        let start = ((y + at) * width + x) * 4;
        bgra[start..start + row]
            .copy_from_slice(unsafe { std::slice::from_raw_parts(data.add(at * pitch), row) });
    }
    for pixel in bgra.as_chunks_mut::<4>().0 {
        pixel[3] = 255;
    }
    Ok(Frame {
        bounds,
        width: width as u32,
        height: height as u32,
        bgra,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compositor_rows_preserve_window_coordinates_without_desktop_border_pixels() {
        let bounds = Rect {
            left: -10,
            top: 20,
            right: -6,
            bottom: 23,
        };
        let surface = Rect {
            left: -9,
            top: 21,
            right: -7,
            bottom: 23,
        };
        let bytes = [
            1, 2, 3, 255, 4, 5, 6, 255, 99, 99, 99, 99, 7, 8, 9, 255, 10, 11, 12, 255, 99, 99, 99,
            99,
        ];
        let frame = copy_surface(bounds, surface, 12, bytes.as_ptr()).unwrap();
        assert_eq!((frame.width, frame.height), (4, 3));
        assert_eq!(&frame.bgra[20..28], &[1, 2, 3, 255, 4, 5, 6, 255]);
        assert_eq!(&frame.bgra[36..44], &[7, 8, 9, 255, 10, 11, 12, 255]);
        assert_eq!(
            &frame.bgra[..16],
            &[0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255]
        );
        assert!(copy_surface(bounds, surface, 4, bytes.as_ptr()).is_err());
    }
}
