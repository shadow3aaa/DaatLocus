use super::{DpiContext, last_error};
use crate::{Error, Rect, Result};
use std::{
    ffi::c_void,
    io::{self, Write},
    ptr,
};
use windows_sys::Win32::Graphics::Gdi::*;

/// Owned top-down BGRA8 pixels in physical display coordinates. GDI capture can be blank
/// for protected/GPU-only content; callers must verify the actual target application.
pub struct Frame {
    pub bounds: Rect,
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

impl Frame {
    /// A simple lossless diagnostic format; production consumers can encode these pixels.
    pub fn write_bmp(&self, writer: &mut impl Write) -> io::Result<()> {
        let expected_length = (self.width as usize)
            .checked_mul(self.height as usize)
            .and_then(|length| length.checked_mul(4));
        if self.width == 0
            || self.height == 0
            || self.width > i32::MAX as u32
            || self.height > i32::MAX as u32
            || expected_length != Some(self.bgra.len())
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid BGRA frame dimensions or buffer",
            ));
        }
        let size = u32::try_from(self.bgra.len())
            .ok()
            .and_then(|n| n.checked_add(54))
            .ok_or_else(|| io::Error::other("BMP exceeds 32-bit file size"))?;
        writer.write_all(b"BM")?;
        writer.write_all(&size.to_le_bytes())?;
        writer.write_all(&[0; 4])?;
        writer.write_all(&54u32.to_le_bytes())?;
        writer.write_all(&40u32.to_le_bytes())?;
        writer.write_all(&self.width.to_le_bytes())?;
        writer.write_all(&(-(self.height as i32)).to_le_bytes())?;
        writer.write_all(&1u16.to_le_bytes())?;
        writer.write_all(&32u16.to_le_bytes())?;
        writer.write_all(&[0; 24])?;
        writer.write_all(&self.bgra)
    }
}

struct CaptureSurface {
    screen: HDC,
    memory: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
}
impl Drop for CaptureSurface {
    fn drop(&mut self) {
        unsafe {
            if !self.previous.is_null() {
                SelectObject(self.memory, self.previous);
            }
            if !self.bitmap.is_null() {
                DeleteObject(self.bitmap);
            }
            if !self.memory.is_null() {
                DeleteDC(self.memory);
            }
            if !self.screen.is_null() {
                ReleaseDC(ptr::null_mut(), self.screen);
            }
        }
    }
}

pub(super) fn capture(bounds: Rect) -> Result<Frame> {
    let _dpi = DpiContext::enter()?;
    let (width, height) = (bounds.width()?, bounds.height()?);
    let length = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(4))
        .ok_or(Error::InvalidState("capture size overflow"))?;
    let mut bgra = Vec::new();
    bgra.try_reserve_exact(length)
        .map_err(|_| Error::InvalidState("cannot allocate capture buffer"))?;
    let mut surface = CaptureSurface {
        screen: ptr::null_mut(),
        memory: ptr::null_mut(),
        bitmap: ptr::null_mut(),
        previous: ptr::null_mut(),
    };
    surface.screen = unsafe { GetDC(ptr::null_mut()) };
    if surface.screen.is_null() {
        return Err(last_error("GetDC"));
    }
    surface.memory = unsafe { CreateCompatibleDC(surface.screen) };
    if surface.memory.is_null() {
        return Err(last_error("CreateCompatibleDC"));
    }
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits: *mut c_void = ptr::null_mut();
    surface.bitmap = unsafe {
        CreateDIBSection(
            surface.screen,
            &info,
            DIB_RGB_COLORS,
            &mut bits,
            ptr::null_mut(),
            0,
        )
    };
    if surface.bitmap.is_null() || bits.is_null() {
        return Err(last_error("CreateDIBSection"));
    }
    surface.previous = unsafe { SelectObject(surface.memory, surface.bitmap) };
    if surface.previous.is_null() || surface.previous as isize == -1 {
        surface.previous = ptr::null_mut();
        return Err(last_error("SelectObject"));
    }
    if unsafe {
        BitBlt(
            surface.memory,
            0,
            0,
            width,
            height,
            surface.screen,
            bounds.left,
            bounds.top,
            SRCCOPY | CAPTUREBLT,
        )
    } == 0
    {
        return Err(last_error("BitBlt"));
    }
    if unsafe { GdiFlush() } == 0 {
        return Err(last_error("GdiFlush"));
    }
    bgra.extend_from_slice(unsafe { std::slice::from_raw_parts(bits.cast::<u8>(), length) });
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
