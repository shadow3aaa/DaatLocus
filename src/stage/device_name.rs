/// OS hostname, independent of environment variables and browser names.
pub fn device_name() -> String {
    name()
        .filter(|name| !name.trim().is_empty())
        .map(|name| {
            let mut bytes = 0;
            name.trim()
                .chars()
                .take_while(|character| {
                    bytes += character.len_utf8();
                    bytes <= 100
                })
                .collect()
        })
        .unwrap_or_else(|| "Daat Locus".into())
}
#[cfg(windows)]
fn name() -> Option<String> {
    use windows_sys::Win32::System::SystemInformation::{
        ComputerNamePhysicalDnsHostname, GetComputerNameExW,
    };
    let mut buffer = [0u16; 256];
    let mut length = buffer.len() as u32;
    // The API writes at most the provided capacity; length excludes the terminator.
    if unsafe {
        GetComputerNameExW(
            ComputerNamePhysicalDnsHostname,
            buffer.as_mut_ptr(),
            &mut length,
        )
    } == 0
    {
        return None;
    }
    String::from_utf16(buffer.get(..length as usize)?).ok()
}
#[cfg(all(unix, not(target_os = "android")))]
fn name() -> Option<String> {
    let mut buffer = [0u8; 256];
    // gethostname receives the exact writable buffer size.
    if unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) } != 0 {
        return None;
    }
    let length = buffer.iter().position(|byte| *byte == 0)?;
    String::from_utf8(buffer[..length].to_vec()).ok()
}
#[cfg(target_os = "android")]
fn name() -> Option<String> {
    unsafe extern "C" {
        fn __system_property_get(
            name: *const std::ffi::c_char,
            value: *mut std::ffi::c_char,
        ) -> std::ffi::c_int;
    }
    let mut value = [0u8; 92];
    let length =
        unsafe { __system_property_get(c"ro.product.model".as_ptr(), value.as_mut_ptr().cast()) };
    if length <= 0 || length as usize >= value.len() {
        return None;
    }
    String::from_utf8(value[..length as usize].to_vec()).ok()
}
#[cfg(not(any(windows, unix)))]
fn name() -> Option<String> {
    None
}
