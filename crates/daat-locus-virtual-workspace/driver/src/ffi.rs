//! Generated WDK ABI. Function-table dispatch follows the WDK inline definitions.
#![allow(
    non_snake_case,
    non_camel_case_types,
    non_upper_case_globals,
    dead_code,
    unused_imports,
    clippy::all
)]
include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
pub use _DRIVER_OBJECT as DRIVER_OBJECT;

#[unsafe(no_mangle)]
pub static IddMinimumVersionRequired: u32 = 4;

// Keep the UMDF loader entry point, which initializes WDF before our DriverEntry.
#[link(
    name = "WdfDriverStubUm",
    kind = "static",
    modifiers = "+whole-archive"
)]
unsafe extern "system" {
    pub fn FxDriverEntryUm(
        a: *mut core::ffi::c_void,
        b: *mut core::ffi::c_void,
        c: *mut core::ffi::c_void,
        d: *mut UNICODE_STRING,
    ) -> NTSTATUS;
}

macro_rules! wdf {
    ($name:ident, $index:ident, $ty:ident, ($($arg:ident: $argty:ty),*) -> $ret:ty) => {
        pub unsafe fn $name($($arg: $argty),*) -> $ret {
            let function: $ty = unsafe { core::mem::transmute(WdfFunctions_02025.add($index as usize).read()) };
            unsafe { function.expect("required UMDF 2.25 function")(WdfDriverGlobals, $($arg),*) }
        }
    };
}
macro_rules! idd {
    ($name:ident, $index:ident, $ty:ident, ($($arg:ident: $argty:ty),*) -> $ret:ty) => {
        pub unsafe fn $name($($arg: $argty),*) -> $ret {
            let function: $ty = unsafe { core::mem::transmute((&raw const IddFunctions).cast::<PFN_IDD_CX>().add($index as usize).read()) };
            unsafe { function.expect("required IddCx 1.4 function")(IddDriverGlobals, $($arg),*) }
        }
    };
}

wdf!(driver_create, _WDFFUNCENUM_WdfDriverCreateTableIndex, PFN_WDFDRIVERCREATE,
    (object: *mut DRIVER_OBJECT, path: *mut UNICODE_STRING, attr: *mut WDF_OBJECT_ATTRIBUTES,
     config: *mut WDF_DRIVER_CONFIG, driver: *mut WDFDRIVER) -> NTSTATUS);
wdf!(set_power, _WDFFUNCENUM_WdfDeviceInitSetPnpPowerEventCallbacksTableIndex, PFN_WDFDEVICEINITSETPNPPOWEREVENTCALLBACKS,
    (init: *mut WDFDEVICE_INIT, callbacks: *mut WDF_PNPPOWER_EVENT_CALLBACKS) -> ());
wdf!(set_files, _WDFFUNCENUM_WdfDeviceInitSetFileObjectConfigTableIndex, PFN_WDFDEVICEINITSETFILEOBJECTCONFIG,
    (init: *mut WDFDEVICE_INIT, config: *mut WDF_FILEOBJECT_CONFIG, attr: *mut WDF_OBJECT_ATTRIBUTES) -> ());
wdf!(device_create, _WDFFUNCENUM_WdfDeviceCreateTableIndex, PFN_WDFDEVICECREATE,
    (init: *mut *mut WDFDEVICE_INIT, attr: *mut WDF_OBJECT_ATTRIBUTES, device: *mut WDFDEVICE) -> NTSTATUS);
wdf!(interface_create, _WDFFUNCENUM_WdfDeviceCreateDeviceInterfaceTableIndex, PFN_WDFDEVICECREATEDEVICEINTERFACE,
    (device: WDFDEVICE, guid: *const GUID, reference: *const UNICODE_STRING) -> NTSTATUS);
wdf!(context, _WDFFUNCENUM_WdfObjectGetTypedContextWorkerTableIndex, PFN_WDFOBJECTGETTYPEDCONTEXTWORKER,
    (object: WDFOBJECT, info: *const WDF_OBJECT_CONTEXT_TYPE_INFO) -> *mut core::ffi::c_void);
wdf!(delete, _WDFFUNCENUM_WdfObjectDeleteTableIndex, PFN_WDFOBJECTDELETE,
    (object: WDFOBJECT) -> ());
wdf!(request_file, _WDFFUNCENUM_WdfRequestGetFileObjectTableIndex, PFN_WDFREQUESTGETFILEOBJECT,
    (request: WDFREQUEST) -> WDFFILEOBJECT);
wdf!(file_device, _WDFFUNCENUM_WdfFileObjectGetDeviceTableIndex, PFN_WDFFILEOBJECTGETDEVICE,
    (file: WDFFILEOBJECT) -> WDFDEVICE);
wdf!(input, _WDFFUNCENUM_WdfRequestRetrieveInputBufferTableIndex, PFN_WDFREQUESTRETRIEVEINPUTBUFFER,
    (request: WDFREQUEST, minimum: usize, buffer: *mut *mut core::ffi::c_void, length: *mut usize) -> NTSTATUS);
wdf!(output, _WDFFUNCENUM_WdfRequestRetrieveOutputBufferTableIndex, PFN_WDFREQUESTRETRIEVEOUTPUTBUFFER,
    (request: WDFREQUEST, minimum: usize, buffer: *mut *mut core::ffi::c_void, length: *mut usize) -> NTSTATUS);
wdf!(complete, _WDFFUNCENUM_WdfRequestCompleteWithInformationTableIndex, PFN_WDFREQUESTCOMPLETEWITHINFORMATION,
    (request: WDFREQUEST, status: NTSTATUS, information: u64) -> ());

idd!(device_init_config, _IDDFUNCENUM_IddCxDeviceInitConfigTableIndex, PFN_IDDCXDEVICEINITCONFIG,
    (init: *mut WDFDEVICE_INIT, config: *const IDD_CX_CLIENT_CONFIG) -> NTSTATUS);
idd!(device_initialize, _IDDFUNCENUM_IddCxDeviceInitializeTableIndex, PFN_IDDCXDEVICEINITIALIZE,
    (device: WDFDEVICE) -> NTSTATUS);
idd!(adapter_init, _IDDFUNCENUM_IddCxAdapterInitAsyncTableIndex, PFN_IDDCXADAPTERINITASYNC,
    (input: *const IDARG_IN_ADAPTER_INIT, output: *mut IDARG_OUT_ADAPTER_INIT) -> NTSTATUS);
idd!(monitor_create, _IDDFUNCENUM_IddCxMonitorCreateTableIndex, PFN_IDDCXMONITORCREATE,
    (adapter: IDDCX_ADAPTER, input: *const IDARG_IN_MONITORCREATE, output: *mut IDARG_OUT_MONITORCREATE) -> NTSTATUS);
idd!(monitor_arrival, _IDDFUNCENUM_IddCxMonitorArrivalTableIndex, PFN_IDDCXMONITORARRIVAL,
    (monitor: IDDCX_MONITOR, output: *mut IDARG_OUT_MONITORARRIVAL) -> NTSTATUS);
idd!(monitor_departure, _IDDFUNCENUM_IddCxMonitorDepartureTableIndex, PFN_IDDCXMONITORDEPARTURE,
    (monitor: IDDCX_MONITOR) -> NTSTATUS);
idd!(chain_device, _IDDFUNCENUM_IddCxSwapChainSetDeviceTableIndex, PFN_IDDCXSWAPCHAINSETDEVICE,
    (chain: IDDCX_SWAPCHAIN, input: *const IDARG_IN_SWAPCHAINSETDEVICE) -> NTSTATUS);
idd!(chain_acquire, _IDDFUNCENUM_IddCxSwapChainReleaseAndAcquireBufferTableIndex, PFN_IDDCXSWAPCHAINRELEASEANDACQUIREBUFFER,
    (chain: IDDCX_SWAPCHAIN, output: *mut IDARG_OUT_RELEASEANDACQUIREBUFFER) -> NTSTATUS);
idd!(chain_finished, _IDDFUNCENUM_IddCxSwapChainFinishedProcessingFrameTableIndex, PFN_IDDCXSWAPCHAINFINISHEDPROCESSINGFRAME,
    (chain: IDDCX_SWAPCHAIN) -> NTSTATUS);
