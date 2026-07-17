#[cfg(windows)]
pub const WINDOWS_CREATE_NO_WINDOW_FLAG: u32 = 0x0800_0000;

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
