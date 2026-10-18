use super::{DpiContext, last_error};
use crate::{ContactPhase, ContactState, Point, Result};
use windows_sys::Win32::{
    Foundation::POINT,
    UI::{
        Controls::{
            CreateSyntheticPointerDevice, DestroySyntheticPointerDevice, HSYNTHETICPOINTERDEVICE,
            POINTER_FEEDBACK_NONE, POINTER_TYPE_INFO, POINTER_TYPE_INFO_0,
        },
        Input::Pointer::*,
        WindowsAndMessaging::PT_TOUCH,
    },
};

/// Low-level Windows synthetic touch device, also usable to debug pointer delivery before
/// installing an IDD driver. Coordinates are physical screen pixels. Windows hit-tests
/// and may activate the receiving application; this is not an HWND-targeted input API.
/// Use VirtualWorkspace for display/window freshness and containment checks.
pub struct TouchDevice {
    handle: HSYNTHETICPOINTERDEVICE,
    contact: ContactState,
}

impl TouchDevice {
    pub fn new() -> Result<Self> {
        let handle = unsafe { CreateSyntheticPointerDevice(PT_TOUCH, 1, POINTER_FEEDBACK_NONE) };
        if handle.is_null() {
            return Err(last_error("CreateSyntheticPointerDevice"));
        }
        Ok(Self {
            handle,
            contact: ContactState::default(),
        })
    }

    /// Submit one frame. Up/Cancel must repeat the last accepted location; move first
    /// to release elsewhere. Success means OS acceptance, not application completion.
    pub fn inject(&mut self, phase: ContactPhase, point: Point) -> Result<()> {
        let _dpi = DpiContext::enter()?;
        self.contact.validate(phase, point)?;
        let flags = match phase {
            ContactPhase::Down => POINTER_FLAG_DOWN | POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT,
            ContactPhase::Move => {
                POINTER_FLAG_UPDATE | POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT
            }
            ContactPhase::Up => POINTER_FLAG_UP,
            ContactPhase::Cancel => POINTER_FLAG_UP | POINTER_FLAG_CANCELED,
        };
        let frame = POINTER_TYPE_INFO {
            r#type: PT_TOUCH,
            Anonymous: POINTER_TYPE_INFO_0 {
                touchInfo: POINTER_TOUCH_INFO {
                    pointerInfo: POINTER_INFO {
                        pointerType: PT_TOUCH,
                        pointerId: 1,
                        pointerFlags: flags,
                        ptPixelLocation: POINT {
                            x: point.x,
                            y: point.y,
                        },
                        // Windows computes routing and pointer metadata. In particular,
                        // hwndTarget is an output fact, never a fabricated routing override.
                        ..Default::default()
                    },
                    ..Default::default()
                },
            },
        };
        if unsafe { InjectSyntheticPointerInput(self.handle, &frame, 1) } == 0 {
            return Err(last_error("InjectSyntheticPointerInput"));
        }
        self.contact.accepted(phase, point);
        Ok(())
    }

    pub fn cancel(&mut self) -> Result<()> {
        if let Some(point) = self.contact.point {
            self.inject(ContactPhase::Cancel, point)?;
        }
        Ok(())
    }
}

impl Drop for TouchDevice {
    fn drop(&mut self) {
        let _ = self.cancel();
        // Device destruction also releases input if explicit cancellation failed.
        unsafe {
            DestroySyntheticPointerDevice(self.handle);
        }
    }
}
