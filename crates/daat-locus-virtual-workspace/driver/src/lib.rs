//! Windows-hosted UMDF 2.25 / IddCx 1.4 display sink. No application injection or IPC server.
mod ffi;
mod frame;
mod modes;
mod trace;

#[allow(dead_code)]
#[path = "../../src/driver_protocol.rs"]
mod driver_protocol;
use driver_protocol as protocol;
#[path = "../../src/driver_leases.rs"]
mod driver_leases;
use driver_leases::{Leases, Token};

use ffi::*;
use std::{
    mem::size_of,
    ptr,
    sync::{
        Mutex, MutexGuard,
        atomic::{AtomicU32, Ordering},
    },
};

const SUCCESS: NTSTATUS = 0;
const INVALID: NTSTATUS = 0xc000000du32 as i32;
const NOT_READY: NTSTATUS = 0xc00000a3u32 as i32;
const BUSY: NTSTATUS = 0xc000009eu32 as i32;
const NOT_SUPPORTED: NTSTATUS = 0xc00000bbu32 as i32;

// WDF contexts contain pointer-sized slots; Rust objects are explicitly constructed
// and destroyed. WDF's zeroed memory must never be treated as an initialized Mutex.
struct TypeInfo(WDF_OBJECT_CONTEXT_TYPE_INFO);
// SAFETY: immutable type descriptors, with only pointers to static storage.
unsafe impl Sync for TypeInfo {}
const fn type_info(name: *const i8, size: usize) -> TypeInfo {
    TypeInfo(WDF_OBJECT_CONTEXT_TYPE_INFO {
        Size: size_of::<WDF_OBJECT_CONTEXT_TYPE_INFO>() as u32,
        ContextName: name,
        ContextSize: size,
        UniqueType: ptr::null(),
        EvtDriverGetUniqueContextType: None,
    })
}
static DEVICE: TypeInfo = type_info(c"DaatLocusDevice".as_ptr(), size_of::<usize>());
static ADAPTER: TypeInfo = type_info(c"DaatLocusAdapter".as_ptr(), size_of::<u32>());
static FILE: TypeInfo = type_info(c"DaatLocusFile".as_ptr(), size_of::<Token>());
static MONITOR: TypeInfo = type_info(c"DaatLocusMonitor".as_ptr(), size_of::<usize>());

struct DeviceState {
    adapter: usize,
    leases: Leases,
    identities: [(u64, u32); protocol::MAX_MONITORS],
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // No recoverable operation panics. A poisoned lock indicates an earlier invariant
    // failure; release builds abort within the UMDF host rather than unwind into WDF.
    mutex.lock().unwrap_or_else(|_| std::process::abort())
}
fn attributes(info: &'static TypeInfo) -> WDF_OBJECT_ATTRIBUTES {
    WDF_OBJECT_ATTRIBUTES {
        Size: size_of::<WDF_OBJECT_ATTRIBUTES>() as u32,
        ContextTypeInfo: &info.0,
        ExecutionLevel: _WDF_EXECUTION_LEVEL_WdfExecutionLevelPassive,
        SynchronizationScope: _WDF_SYNCHRONIZATION_SCOPE_WdfSynchronizationScopeNone,
        ..Default::default()
    }
}
unsafe fn slot<T>(object: WDFOBJECT, info: &'static TypeInfo) -> *mut T {
    unsafe { context(object, &info.0).cast() }
}
unsafe fn state<'a>(device: WDFDEVICE) -> &'a Mutex<DeviceState> {
    // SAFETY: WDF keeps the device and its context alive throughout each callback.
    unsafe { &**slot::<*mut Mutex<DeviceState>>(device.cast(), &DEVICE) }
}

#[unsafe(no_mangle)]
/// # Safety
/// Called only by the UMDF loader stub after it binds WDF/IddCx function tables.
/// Both pointers must be the initialized objects provided by that stub.
pub unsafe extern "system" fn DriverEntry(
    object: *mut ffi::DRIVER_OBJECT,
    path: *mut ffi::UNICODE_STRING,
) -> ffi::NTSTATUS {
    trace::register();
    let mut config = WDF_DRIVER_CONFIG {
        Size: size_of::<WDF_DRIVER_CONFIG>() as u32,
        EvtDriverDeviceAdd: Some(add_device),
        EvtDriverUnload: Some(unload),
        ..Default::default()
    };
    let status =
        unsafe { driver_create(object, path, ptr::null_mut(), &mut config, ptr::null_mut()) };
    trace::status("driver create", status);
    if status < 0 {
        trace::unregister();
    }
    status
}

unsafe extern "C" fn unload(_: WDFDRIVER) {
    trace::unregister();
}

unsafe extern "C" fn add_device(_: WDFDRIVER, mut init: PWDFDEVICE_INIT) -> NTSTATUS {
    let mut power = WDF_PNPPOWER_EVENT_CALLBACKS {
        Size: size_of::<WDF_PNPPOWER_EVENT_CALLBACKS>() as u32,
        EvtDeviceD0Entry: Some(power_on),
        EvtDeviceD0Exit: Some(power_off),
        ..Default::default()
    };
    let mut files = WDF_FILEOBJECT_CONFIG {
        Size: size_of::<WDF_FILEOBJECT_CONFIG>() as u32,
        EvtFileCleanup: Some(file_cleanup),
        AutoForwardCleanupClose: _WDF_TRI_STATE_WdfFalse,
        FileObjectClass: _WDF_FILEOBJECT_CLASS_WdfFileObjectWdfCannotUseFsContexts,
        ..Default::default()
    };
    let mut file_attr = attributes(&FILE);
    let callbacks = IDD_CX_CLIENT_CONFIG {
        Size: size_of::<IDD_CX_CLIENT_CONFIG>() as u32,
        EvtIddCxDeviceIoControl: Some(io_control),
        EvtIddCxAdapterInitFinished: Some(adapter_ready),
        EvtIddCxAdapterCommitModes: Some(modes::commit),
        EvtIddCxParseMonitorDescription: Some(modes::parse),
        EvtIddCxMonitorGetDefaultDescriptionModes: Some(modes::description),
        EvtIddCxMonitorQueryTargetModes: Some(modes::target),
        EvtIddCxMonitorAssignSwapChain: Some(assign_chain),
        EvtIddCxMonitorUnassignSwapChain: Some(unassign_chain),
        ..Default::default()
    };
    let mut attr = attributes(&DEVICE);
    attr.EvtDestroyCallback = Some(device_destroy);
    let mut device = ptr::null_mut();
    // SAFETY: WDF owns init; all callback/config structures live through registration.
    unsafe {
        set_power(init, &mut power);
        set_files(init, &mut files, &mut file_attr);
        let status = device_init_config(init, &callbacks);
        if status < 0 {
            return status;
        }
        let status = device_create(&mut init, &mut attr, &mut device);
        if status < 0 {
            return status;
        }
        *slot(device.cast(), &DEVICE) = Box::into_raw(Box::new(Mutex::new(DeviceState {
            adapter: 0,
            leases: Leases::default(),
            identities: [(0, 0); protocol::MAX_MONITORS],
        })));
        let status = device_initialize(device);
        if status < 0 {
            return status;
        }
        let uuid = protocol::INTERFACE_GUID;
        let guid = GUID {
            Data1: (uuid >> 96) as u32,
            Data2: (uuid >> 80) as u16,
            Data3: (uuid >> 64) as u16,
            Data4: (uuid as u64).to_be_bytes(),
        };
        let status = interface_create(device, &guid, ptr::null());
        if status < 0 {
            return status;
        }
        // IddCx owns the default WDF queue and forwards client IOCTLs through
        // EvtIddCxDeviceIoControl. A second default queue never receives them.
        SUCCESS
    }
}

unsafe extern "C" fn device_destroy(object: WDFOBJECT) {
    // All child callbacks have finished before WDF destroys their parent context.
    unsafe {
        let pointer = slot::<*mut Mutex<DeviceState>>(object, &DEVICE);
        if !(*pointer).is_null() {
            drop(Box::from_raw(*pointer));
            *pointer = ptr::null_mut();
        }
    }
}

unsafe extern "C" fn power_on(device: WDFDEVICE, _: WDF_POWER_DEVICE_STATE) -> NTSTATUS {
    let mutex = unsafe { state(device) };
    let mut state = lock(mutex);
    if state.adapter != 0 {
        return SUCCESS;
    }
    let mut version = IDDCX_ENDPOINT_VERSION {
        Size: size_of::<IDDCX_ENDPOINT_VERSION>() as u32,
        MajorVer: 1,
        ..Default::default()
    };
    let mut caps = IDDCX_ADAPTER_CAPS {
        Size: size_of::<IDDCX_ADAPTER_CAPS>() as u32,
        MaxMonitorsSupported: protocol::MAX_MONITORS as u32,
        ..Default::default()
    };
    caps.EndPointDiagnostics.Size = size_of_val(&caps.EndPointDiagnostics) as u32;
    caps.EndPointDiagnostics.GammaSupport =
        IDDCX_FEATURE_IMPLEMENTATION_IDDCX_FEATURE_IMPLEMENTATION_NONE;
    caps.EndPointDiagnostics.TransmissionType =
        IDDCX_TRANSMISSION_TYPE_IDDCX_TRANSMISSION_TYPE_WIRED_OTHER;
    caps.EndPointDiagnostics.pEndPointFriendlyName =
        windows::core::w!("Daat Locus Virtual Workspace").as_ptr();
    caps.EndPointDiagnostics.pEndPointManufacturerName = windows::core::w!("Daat Locus").as_ptr();
    caps.EndPointDiagnostics.pEndPointModelName = windows::core::w!("Virtual Display").as_ptr();
    caps.EndPointDiagnostics.pFirmwareVersion = &mut version;
    caps.EndPointDiagnostics.pHardwareVersion = &mut version;
    let mut attr = attributes(&ADAPTER);
    let input = IDARG_IN_ADAPTER_INIT {
        WdfDevice: device,
        pCaps: &mut caps,
        ObjectAttributes: &mut attr,
    };
    let mut output = IDARG_OUT_ADAPTER_INIT::default();
    let status = unsafe { adapter_init(&input, &mut output) };
    trace::status("adapter init", status);
    if status >= 0 {
        state.adapter = output.AdapterObject as usize;
    }
    status
}

unsafe extern "C" fn adapter_ready(
    adapter: IDDCX_ADAPTER,
    input: *const IDARG_IN_ADAPTER_INIT_FINISHED,
) -> NTSTATUS {
    // The zeroed WDF context is an aligned integer: 0=pending, 1=ready, 2=failed.
    trace::status("adapter ready", unsafe { (*input).AdapterInitStatus });
    // No context initialization after AdapterInitAsync is needed, so even a very
    // early completion cannot race an uninitialized Rust pointer or take our lock.
    let ready = unsafe { AtomicU32::from_ptr(slot(adapter.cast(), &ADAPTER)) };
    ready.store(
        if unsafe { (*input).AdapterInitStatus >= 0 } {
            1
        } else {
            2
        },
        Ordering::Release,
    );
    SUCCESS
}

unsafe extern "C" fn power_off(device: WDFDEVICE, _: WDF_POWER_DEVICE_STATE) -> NTSTATUS {
    trace::status("power off", 0);
    let mut state = lock(unsafe { state(device) });
    let adapter = std::mem::take(&mut state.adapter);
    let monitors = state.leases.retire();
    // Serialize departure and parent deletion with file cleanup. Otherwise parent
    // removal could delete a monitor that file cleanup has taken out of the pool.
    // Reentrant IddCx callbacks touch only adapter atomics / monitor-local pumps.
    for monitor in monitors.into_iter().filter(|monitor| *monitor != 0) {
        unsafe {
            monitor_departure(monitor as IDDCX_MONITOR);
        }
    }
    if adapter != 0 {
        unsafe {
            delete(adapter as WDFOBJECT);
        }
    }
    SUCCESS
}

unsafe extern "C" fn io_control(
    _: WDFDEVICE,
    request: WDFREQUEST,
    output_len: usize,
    input_len: usize,
    code: u32,
) {
    // No callback unwinds into Windows. Normal failures use NTSTATUS; allocation or
    // invariant failures abort the isolated UMDF host in the release profile.
    let result = unsafe { create_monitor(request, output_len, input_len, code) };
    trace::status("IOCTL", code as i32);
    trace::status("IOCTL completion", result.0);
    unsafe {
        complete(request, result.0, result.1);
    }
}

unsafe fn create_monitor(
    request: WDFREQUEST,
    output_len: usize,
    input_len: usize,
    code: u32,
) -> (NTSTATUS, u64) {
    if code != protocol::CREATE && code != protocol::QUERY {
        return (NOT_SUPPORTED, 0);
    }
    if input_len != protocol::REQUEST_BYTES || output_len < protocol::RESPONSE_BYTES {
        return (INVALID, 0);
    }
    let mut input = ptr::null_mut();
    let mut output = ptr::null_mut();
    // METHOD_BUFFERED buffers may alias: copy the request before writing any response.
    let status = unsafe { ffi::input(request, input_len, &mut input, ptr::null_mut()) };
    trace::status("input buffer", status);
    if status < 0 {
        return (status, 0);
    }
    let bytes = unsafe { std::slice::from_raw_parts(input.cast::<u8>(), input_len) };
    let Some(container) = protocol::parse_request(bytes) else {
        return (INVALID, 0);
    };
    let status = unsafe {
        ffi::output(
            request,
            protocol::RESPONSE_BYTES,
            &mut output,
            ptr::null_mut(),
        )
    };
    if status < 0 {
        return (status, 0);
    }
    let file = unsafe { request_file(request) };
    if file.is_null() {
        return (INVALID, 0);
    }
    let device = unsafe { file_device(file) };
    let mut state = lock(unsafe { state(device) });
    let lease = unsafe { &mut *slot::<Token>(file.cast(), &FILE) };
    if lease.is_closed() {
        return (INVALID, 0);
    }
    if state.adapter == 0 {
        return (NOT_READY, 0);
    }
    let ready = unsafe { AtomicU32::from_ptr(slot(state.adapter as WDFOBJECT, &ADAPTER)) };
    match ready.load(Ordering::Acquire) {
        1 => (),
        2 => return (0xc0000182u32 as i32, 0), // STATUS_DEVICE_CONFIGURATION_ERROR
        _ => return (NOT_READY, 0),
    }
    if code == protocol::QUERY {
        let Some(connector) = state.leases.lookup(lease) else {
            return (INVALID, 0);
        };
        let (adapter, target) = state.identities[connector];
        let bytes = protocol::response(adapter, target);
        unsafe {
            ptr::copy_nonoverlapping(bytes.as_ptr(), output.cast(), bytes.len());
        }
        return (SUCCESS, protocol::RESPONSE_BYTES as u64);
    }
    if lease.is_assigned() {
        return (INVALID, 0);
    }
    let Some(connector) = state.leases.vacant() else {
        return (BUSY, 0);
    };
    let id = u128::from_be_bytes(container);
    let mut info = IDDCX_MONITOR_INFO {
        Size: size_of::<IDDCX_MONITOR_INFO>() as u32,
        MonitorType: DISPLAYCONFIG_VIDEO_OUTPUT_TECHNOLOGY_DISPLAYCONFIG_OUTPUT_TECHNOLOGY_OTHER,
        ConnectorIndex: connector as u32,
        MonitorContainerId: GUID {
            Data1: (id >> 96) as u32,
            Data2: (id >> 80) as u16,
            Data3: (id >> 64) as u16,
            Data4: (id as u64).to_be_bytes(),
        },
        ..Default::default()
    };
    info.MonitorDescription.Size = size_of_val(&info.MonitorDescription) as u32;
    info.MonitorDescription.Type =
        IDDCX_MONITOR_DESCRIPTION_TYPE_IDDCX_MONITOR_DESCRIPTION_TYPE_EDID;
    let mut attr = attributes(&MONITOR);
    attr.EvtCleanupCallback = Some(monitor_cleanup);
    let create = IDARG_IN_MONITORCREATE {
        pMonitorInfo: &mut info,
        ObjectAttributes: &mut attr,
    };
    let mut created = IDARG_OUT_MONITORCREATE::default();
    let status = unsafe { monitor_create(state.adapter as IDDCX_ADAPTER, &create, &mut created) };
    trace::status("monitor create", status);
    if status < 0 {
        return (status, 0);
    }
    unsafe {
        *slot(created.MonitorObject.cast(), &MONITOR) =
            Box::into_raw(Box::new(Mutex::new(None::<frame::FramePump>)));
    }
    let mut arrival = IDARG_OUT_MONITORARRIVAL::default();
    let status = unsafe { monitor_arrival(created.MonitorObject, &mut arrival) };
    trace::status("monitor arrival", status);
    if status < 0 {
        unsafe {
            delete(created.MonitorObject.cast());
        }
        return (status, 0);
    }
    *lease = state
        .leases
        .assign(connector, created.MonitorObject as usize);
    let adapter = (u64::from(arrival.OsAdapterLuid.HighPart as u32) << 32)
        | u64::from(arrival.OsAdapterLuid.LowPart);
    state.identities[connector] = (adapter, arrival.OsTargetId);
    let bytes = protocol::response(adapter, arrival.OsTargetId);
    unsafe {
        ptr::copy_nonoverlapping(bytes.as_ptr(), output.cast(), bytes.len());
    }
    (SUCCESS, protocol::RESPONSE_BYTES as u64)
}

unsafe extern "C" fn file_cleanup(file: WDFFILEOBJECT) {
    trace::status("file cleanup", 0);
    let device = unsafe { file_device(file) };
    let mut state = lock(unsafe { state(device) });
    let lease = unsafe { &mut *slot::<Token>(file.cast(), &FILE) };
    let monitor = state.leases.release(lease);
    if let Some(monitor) = monitor {
        unsafe {
            monitor_departure(monitor as IDDCX_MONITOR);
        }
    }
}

unsafe fn pump<'a>(monitor: IDDCX_MONITOR) -> &'a Mutex<Option<frame::FramePump>> {
    unsafe { &**slot::<*mut Mutex<Option<frame::FramePump>>>(monitor.cast(), &MONITOR) }
}
unsafe extern "C" fn assign_chain(
    monitor: IDDCX_MONITOR,
    input: *const IDARG_IN_SETSWAPCHAIN,
) -> NTSTATUS {
    let mut pump = lock(unsafe { pump(monitor) });
    drop(pump.take());
    *pump = unsafe { frame::FramePump::start(&*input) };
    SUCCESS
}
unsafe extern "C" fn unassign_chain(monitor: IDDCX_MONITOR) -> NTSTATUS {
    drop(lock(unsafe { pump(monitor) }).take());
    SUCCESS
}
unsafe extern "C" fn monitor_cleanup(object: WDFOBJECT) {
    unsafe {
        let pointer = slot::<*mut Mutex<Option<frame::FramePump>>>(object, &MONITOR);
        if !(*pointer).is_null() {
            drop(Box::from_raw(*pointer));
            *pointer = ptr::null_mut();
        }
    }
}
