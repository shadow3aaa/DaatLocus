#[cfg(windows)]
pub const WINDOWS_CREATE_NO_WINDOW_FLAG: u32 = 0x0800_0000;

/// Windows PowerShell encodes text written to a redirected stdout/stderr with the active code
/// page unless the console is already UTF-8, which garbles non-ASCII output on systems where
/// the UTF-8 system option is disabled (for example GBK with code page 936). Sessions always
/// decode child output as UTF-8, so the shell has to emit UTF-8 before anything else runs.
#[cfg(windows)]
pub const POWERSHELL_UTF8_BOOTSTRAP: &str = "try { [Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false) } catch { }; \
try { [Console]::InputEncoding = [System.Text.UTF8Encoding]::new($false) } catch { }; \
$OutputEncoding = [System.Text.UTF8Encoding]::new($false)";

#[cfg(not(windows))]
pub const POWERSHELL_UTF8_BOOTSTRAP: &str = "";

pub fn apply_no_window(command: &mut std::process::Command) {
    apply_no_window_with_flags(command, 0);
}

#[cfg(windows)]
pub fn apply_no_window_with_flags(command: &mut std::process::Command, flags: u32) {
    use std::os::windows::process::CommandExt;

    command.creation_flags(flags | WINDOWS_CREATE_NO_WINDOW_FLAG);
}

#[cfg(not(windows))]
pub(crate) fn apply_no_window_with_flags(_command: &mut std::process::Command, _flags: u32) {}

#[cfg(all(test, windows))]
mod tests {
    use super::WINDOWS_CREATE_NO_WINDOW_FLAG;

    #[test]
    fn windows_no_window_flag_matches_winapi_value() {
        assert_eq!(WINDOWS_CREATE_NO_WINDOW_FLAG, 0x0800_0000);
    }
}
