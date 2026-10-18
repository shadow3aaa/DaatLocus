//! A bounded-lifetime Direct3D sink. Each successful acquisition releases its COM
//! surface, and every worker exit deletes the abandoned swapchain exactly once.
use crate::ffi;
use std::thread::JoinHandle;
use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE, HMODULE, LUID, WAIT_OBJECT_0},
        Graphics::{Direct3D::D3D_DRIVER_TYPE_UNKNOWN, Direct3D11::*, Dxgi::*},
        System::Threading::{CreateEventW, SetEvent, WaitForMultipleObjects, WaitForSingleObject},
    },
    core::{IUnknown, Interface},
};

pub struct FramePump {
    stop: HANDLE,
    thread: Option<JoinHandle<()>>,
}
// SAFETY: Windows event handles are cross-thread; Drop signals then joins before close.
unsafe impl Send for FramePump {}

impl FramePump {
    pub unsafe fn start(input: &ffi::IDARG_IN_SETSWAPCHAIN) -> Option<Self> {
        let chain = input.hSwapChain as usize;
        let stop = match unsafe { CreateEventW(None, true, false, None) } {
            Ok(stop) => stop,
            Err(_) => {
                unsafe {
                    ffi::delete(chain as ffi::WDFOBJECT);
                }
                return None;
            }
        };
        let stop_value = stop.0 as usize;
        let available = input.hNextSurfaceAvailable as usize;
        let luid = (
            input.RenderAdapterLuid.LowPart,
            input.RenderAdapterLuid.HighPart,
        );
        let thread = std::thread::Builder::new()
            .name("dl-idd-frame".into())
            .spawn(move || {
                // SAFETY: IddCx gives the driver ownership of this swapchain. Unassign
                // joins this worker before releasing any of its borrowed event handles.
                unsafe {
                    let mut device = None;
                    let result = consume(chain, stop_value, available, luid, &mut device);
                    if let Err(error) = result {
                        crate::trace::status("frame consumer", error.code().0);
                    }
                    ffi::delete(chain as ffi::WDFOBJECT);
                    // Keep the DXGI device alive until the swapchain is deleted,
                    // including exits caused by D3D/device or acquisition failure.
                    drop(device);
                }
            });
        match thread {
            Ok(thread) => Some(Self {
                stop,
                thread: Some(thread),
            }),
            Err(_) => {
                unsafe {
                    let _ = CloseHandle(stop);
                    ffi::delete(chain as ffi::WDFOBJECT);
                }
                None
            }
        }
    }
}
impl Drop for FramePump {
    fn drop(&mut self) {
        unsafe {
            let _ = SetEvent(self.stop);
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        unsafe {
            let _ = CloseHandle(self.stop);
        }
    }
}

unsafe fn consume(
    chain: usize,
    stop: usize,
    available: usize,
    luid: (u32, i32),
    keep_alive: &mut Option<IDXGIDevice>,
) -> windows::core::Result<()> {
    unsafe {
        let factory: IDXGIFactory4 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0))?;
        let adapter: IDXGIAdapter1 = factory.EnumAdapterByLuid(LUID {
            LowPart: luid.0,
            HighPart: luid.1,
        })?;
        let mut device = None;
        D3D11CreateDevice(
            &adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )?;
        let device: IDXGIDevice = device
            .ok_or_else(windows::core::Error::from_thread)?
            .cast()?;
        let input = ffi::IDARG_IN_SWAPCHAINSETDEVICE {
            pDevice: device.as_raw().cast(),
        };
        *keep_alive = Some(device.clone());
        if ffi::chain_device(chain as ffi::IDDCX_SWAPCHAIN, &input) < 0 {
            return Ok(());
        }
        let events = [HANDLE(stop as *mut _), HANDLE(available as *mut _)];
        loop {
            if WaitForSingleObject(events[0], 0) == WAIT_OBJECT_0 {
                break;
            }
            let mut frame = ffi::IDARG_OUT_RELEASEANDACQUIREBUFFER::default();
            let status = ffi::chain_acquire(chain as ffi::IDDCX_SWAPCHAIN, &mut frame);
            if status as u32 == 0x8000000a {
                // E_PENDING: wait for a frame or teardown.
                let wait = WaitForMultipleObjects(&events, false, u32::MAX);
                if wait.0 == WAIT_OBJECT_0.0 + 1 {
                    continue;
                }
                break;
            }
            if status < 0 {
                break;
            }
            if !frame.MetaData.pSurface.is_null() {
                drop(IUnknown::from_raw(frame.MetaData.pSurface.cast()));
            }
            if ffi::chain_finished(chain as ffi::IDDCX_SWAPCHAIN) < 0 {
                break;
            }
        }
    }
    Ok(())
}
