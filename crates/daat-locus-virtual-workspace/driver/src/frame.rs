//! A bounded-lifetime Direct3D sink. Each successful acquisition releases its COM
//! surface, and every worker exit deletes the abandoned swapchain exactly once.
use crate::{ffi, trace};
use std::{fmt::Arguments, thread::JoinHandle};
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
    trace: FrameTrace,
}

#[derive(Clone, Copy)]
struct FrameTrace {
    id: u64,
    monitor: usize,
    chain: usize,
}

#[derive(Debug, PartialEq, Eq)]
enum ExitReason {
    StopRequested,
    Abandoned,
    DeviceStopped,
    ApiFailure,
}
impl ExitReason {
    fn classify(status: i32, stopping: bool) -> Self {
        if stopping {
            return Self::StopRequested;
        }
        match status as u32 {
            0xc0262012 => Self::Abandoned,
            0xc0262013 => Self::DeviceStopped,
            _ => Self::ApiFailure,
        }
    }
    fn name(&self) -> &'static str {
        match self {
            Self::StopRequested => "stop_requested",
            Self::Abandoned => "swapchain_abandoned",
            Self::DeviceStopped => "graphics_device_stopped",
            Self::ApiFailure => "api_failure",
        }
    }
}
impl FrameTrace {
    fn event(self, stage: &str, details: Arguments<'_>) {
        trace::event(
            stage,
            format_args!(
                "pump={} monitor=0x{:x} chain=0x{:x} {details}",
                self.id, self.monitor, self.chain
            ),
        );
    }
    fn result(self, stage: &str, status: i32) {
        trace::result(
            stage,
            "hresult",
            status,
            format_args!(
                "pump={} monitor=0x{:x} chain=0x{:x}",
                self.id, self.monitor, self.chain
            ),
        );
    }
    fn checked<T>(self, stage: &str, result: windows::core::Result<T>) -> windows::core::Result<T> {
        self.result(
            stage,
            result.as_ref().err().map_or(0, |error| error.code().0),
        );
        result
    }
    fn failure(
        self,
        stage: &str,
        status: i32,
        stop: usize,
        device: Option<&IDXGIDevice>,
    ) -> &'static str {
        // IddCx returns HRESULT here. Abandoning/stopping a chain and an API
        // completing after our stop signal are expected teardown, not failures.
        let reason = ExitReason::classify(
            status,
            unsafe { WaitForSingleObject(HANDLE(stop as *mut _), 0) } == WAIT_OBJECT_0,
        );
        if reason == ExitReason::ApiFailure {
            self.result(stage, status);
            // Only query extra D3D diagnostics while a trace is enabled. No device
            // rebuild/retry or change to the frame worker's existing control flow.
            if trace::enabled(2)
                && let Some(device) = device
            {
                match device.cast::<ID3D11Device>() {
                    Ok(device) => {
                        let reason = unsafe { device.GetDeviceRemovedReason() };
                        self.result(
                            "d3d.device_removed_reason",
                            reason.err().map_or(0, |error| error.code().0),
                        );
                    }
                    Err(error) => self.result("d3d.diagnostic_device_cast", error.code().0),
                }
            }
        } else {
            self.event(
                stage,
                format_args!(
                    "status_kind=hresult status=0x{:08x} reason={}",
                    status as u32,
                    reason.name()
                ),
            );
        }
        reason.name()
    }
    unsafe fn delete_chain(self) {
        self.event("swapchain.delete.begin", format_args!(""));
        unsafe {
            ffi::delete(self.chain as ffi::WDFOBJECT);
        }
        self.event("swapchain.delete.end", format_args!(""));
    }
}
// SAFETY: Windows event handles are cross-thread; Drop signals then joins before close.
unsafe impl Send for FramePump {}

impl FramePump {
    pub unsafe fn start(monitor: usize, input: &ffi::IDARG_IN_SETSWAPCHAIN) -> Option<Self> {
        let chain = input.hSwapChain as usize;
        let trace = FrameTrace {
            id: trace::next_id(),
            monitor,
            chain,
        };
        trace.event(
            "frame_pump.start",
            format_args!(
                "render_luid={:08x}{:08x}",
                input.RenderAdapterLuid.HighPart as u32, input.RenderAdapterLuid.LowPart
            ),
        );
        let stop = match unsafe { CreateEventW(None, true, false, None) } {
            Ok(stop) => stop,
            Err(error) => {
                trace.result("frame_pump.create_stop_event", error.code().0);
                unsafe {
                    trace.delete_chain();
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
                    trace.event("frame_worker.begin", format_args!(""));
                    match consume(trace, stop_value, available, luid, &mut device) {
                        Ok(reason) => {
                            trace.event("frame_worker.end", format_args!("reason={reason}"))
                        }
                        Err(error) => trace.result("frame_worker.end", error.code().0),
                    }
                    trace.delete_chain();
                    // Keep the DXGI device alive until the swapchain is deleted,
                    // including exits caused by D3D/device or acquisition failure.
                    drop(device);
                    trace.event("frame_worker.resources_released", format_args!(""));
                }
            });
        match thread {
            Ok(thread) => Some(Self {
                stop,
                thread: Some(thread),
                trace,
            }),
            Err(error) => {
                trace::error(
                    "frame_worker.spawn",
                    format_args!("pump={} raw_os_error={:?}", trace.id, error.raw_os_error()),
                );
                unsafe {
                    trace
                        .checked("frame_pump.close_stop_event", CloseHandle(stop))
                        .ok();
                    trace.delete_chain();
                }
                None
            }
        }
    }
}
impl Drop for FramePump {
    fn drop(&mut self) {
        self.trace.event("frame_pump.stop.begin", format_args!(""));
        unsafe {
            self.trace
                .checked("frame_pump.signal_stop", SetEvent(self.stop))
                .ok();
        }
        if let Some(thread) = self.thread.take() {
            self.trace
                .event("frame_worker.join.begin", format_args!(""));
            if thread.join().is_err() {
                trace::error(
                    "frame_worker.join",
                    format_args!("pump={} reason=worker_panicked", self.trace.id),
                );
            }
            self.trace.event("frame_worker.join.end", format_args!(""));
        }
        unsafe {
            self.trace
                .checked("frame_pump.close_stop_event", CloseHandle(self.stop))
                .ok();
        }
        self.trace.event("frame_pump.stop.end", format_args!(""));
    }
}

unsafe fn consume(
    trace: FrameTrace,
    stop: usize,
    available: usize,
    luid: (u32, i32),
    keep_alive: &mut Option<IDXGIDevice>,
) -> windows::core::Result<&'static str> {
    unsafe {
        let factory: IDXGIFactory4 = trace.checked(
            "dxgi.create_factory",
            CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0)),
        )?;
        let adapter: IDXGIAdapter1 = trace.checked(
            "dxgi.adapter_by_luid",
            factory.EnumAdapterByLuid(LUID {
                LowPart: luid.0,
                HighPart: luid.1,
            }),
        )?;
        let mut device = None;
        trace.checked(
            "d3d.create_device",
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
            ),
        )?;
        let device = trace.checked(
            "d3d.device_output",
            device.ok_or_else(windows::core::Error::from_thread),
        )?;
        let device: IDXGIDevice = trace.checked("d3d.dxgi_device_cast", device.cast())?;
        let input = ffi::IDARG_IN_SWAPCHAINSETDEVICE {
            pDevice: device.as_raw().cast(),
        };
        *keep_alive = Some(device.clone());
        let status = ffi::chain_device(trace.chain as ffi::IDDCX_SWAPCHAIN, &input);
        if status < 0 {
            return Ok(trace.failure("swapchain.set_device", status, stop, Some(&device)));
        }
        trace.result("swapchain.set_device", status);
        let events = [HANDLE(stop as *mut _), HANDLE(available as *mut _)];
        loop {
            let wait = WaitForSingleObject(events[0], 0);
            if wait == WAIT_OBJECT_0 {
                return Ok("stop_requested");
            }
            let mut frame = ffi::IDARG_OUT_RELEASEANDACQUIREBUFFER::default();
            let status = ffi::chain_acquire(trace.chain as ffi::IDDCX_SWAPCHAIN, &mut frame);
            if status as u32 == 0x8000000a {
                // E_PENDING: wait for a frame or teardown.
                let wait = WaitForMultipleObjects(&events, false, u32::MAX);
                if wait.0 == WAIT_OBJECT_0.0 + 1 {
                    continue;
                }
                if wait == WAIT_OBJECT_0 {
                    return Ok("stop_requested");
                }
                if wait == windows::Win32::Foundation::WAIT_FAILED {
                    trace.result(
                        "frame_pump.wait_surface",
                        windows::core::Error::from_thread().code().0,
                    );
                } else {
                    trace::error(
                        "frame_pump.wait_surface",
                        format_args!("pump={} wait_result=0x{:08x}", trace.id, wait.0),
                    );
                }
                return Ok("wait_failed");
            }
            if status < 0 {
                return Ok(trace.failure("swapchain.acquire", status, stop, Some(&device)));
            }
            if !frame.MetaData.pSurface.is_null() {
                drop(IUnknown::from_raw(frame.MetaData.pSurface.cast()));
            }
            let status = ffi::chain_finished(trace.chain as ffi::IDDCX_SWAPCHAIN);
            if status < 0 {
                return Ok(trace.failure("swapchain.finish_frame", status, stop, Some(&device)));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ExitReason;

    #[test]
    fn swapchain_teardown_statuses_and_stop_races_are_not_api_failures() {
        for (status, expected) in [
            (0xc0262012u32, ExitReason::Abandoned),
            (0xc0262013, ExitReason::DeviceStopped),
            (0x80004005, ExitReason::ApiFailure),
            (0x887a0005, ExitReason::ApiFailure), // DXGI_ERROR_DEVICE_REMOVED
        ] {
            assert_eq!(ExitReason::classify(status as i32, false), expected);
            assert_eq!(
                ExitReason::classify(status as i32, true),
                ExitReason::StopRequested
            );
        }
    }
}
