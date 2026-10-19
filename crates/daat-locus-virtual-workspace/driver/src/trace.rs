//! Opt-in ETW diagnostics. Events contain driver objects and status codes, never app data.
use std::{
    fmt::Arguments,
    sync::atomic::{AtomicI64, AtomicU64, Ordering},
};
use windows::{
    Win32::System::Diagnostics::Etw::{
        EventProviderEnabled, EventRegister, EventUnregister, EventWriteString, REGHANDLE,
    },
    core::{GUID, PCWSTR},
};

// Capture with logman -p {70b75adc-3a20-4a68-99a4-bc5249203ca0}.
const PROVIDER: GUID = GUID::from_u128(0x70b75adc_3a20_4a68_99a4_bc5249203ca0);
static HANDLE: AtomicI64 = AtomicI64::new(0);
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

pub fn enabled(level: u8) -> bool {
    let handle = HANDLE.load(Ordering::Acquire);
    handle != 0 && unsafe { EventProviderEnabled(REGHANDLE(handle), level, 0) }
}

pub fn register() {
    let mut handle = REGHANDLE::default();
    if unsafe { EventRegister(&PROVIDER, None, None, &mut handle) } == 0 {
        HANDLE.store(handle.0, Ordering::Release);
    }
}

pub fn unregister() {
    let handle = HANDLE.swap(0, Ordering::AcqRel);
    if handle != 0 {
        unsafe { EventUnregister(REGHANDLE(handle)) };
    }
}

pub fn status(stage: &str, status: i32) {
    result(stage, "ntstatus", status, format_args!(""));
}

pub fn event(stage: &str, details: Arguments<'_>) {
    write(4, format_args!("event={stage} {details}"));
}

pub fn error(stage: &str, details: Arguments<'_>) {
    write(2, format_args!("event={stage} {details}"));
}

pub fn result(stage: &str, kind: &str, status: i32, details: Arguments<'_>) {
    write(
        if status < 0 { 2 } else { 4 },
        format_args!(
            "event={stage} status_kind={kind} status=0x{:08x} {details}",
            status as u32
        ),
    );
}

fn write(level: u8, message: Arguments<'_>) {
    let handle = HANDLE.load(Ordering::Acquire);
    if handle == 0 || !unsafe { EventProviderEnabled(REGHANDLE(handle), level, 0) } {
        return;
    }
    // Do not allocate/format when no diagnostic session is listening. No event is
    // emitted per frame or per E_PENDING wait; capture size is bounded by logman.
    let message: Vec<u16> = format!("{message}").encode_utf16().chain(Some(0)).collect();
    unsafe { EventWriteString(REGHANDLE(handle), level, 0, PCWSTR(message.as_ptr())) };
}
