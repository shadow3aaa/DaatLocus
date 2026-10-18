//! Opt-in ETW diagnostics. Events contain stages and status codes, never app data.
use std::sync::atomic::{AtomicI64, Ordering};
use windows::{
    Win32::System::Diagnostics::Etw::{
        EventRegister, EventUnregister, EventWriteString, REGHANDLE,
    },
    core::{GUID, PCWSTR},
};

// Capture with logman -p {70b75adc-3a20-4a68-99a4-bc5249203ca0}.
const PROVIDER: GUID = GUID::from_u128(0x70b75adc_3a20_4a68_99a4_bc5249203ca0);
static HANDLE: AtomicI64 = AtomicI64::new(0);

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
    let handle = HANDLE.load(Ordering::Acquire);
    if handle == 0 {
        return;
    }
    let message: Vec<u16> = format!("{stage}: 0x{:08x}", status as u32)
        .encode_utf16()
        .chain(Some(0))
        .collect();
    unsafe { EventWriteString(REGHANDLE(handle), 4, 0, PCWSTR(message.as_ptr())) };
}
