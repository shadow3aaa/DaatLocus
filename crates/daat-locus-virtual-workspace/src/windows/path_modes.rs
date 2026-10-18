//! DisplayConfig uses packed 16-bit indices on virtual-aware paths.
use windows_sys::Win32::{
    Devices::Display::*, Graphics::Gdi::DISPLAYCONFIG_PATH_SUPPORT_VIRTUAL_MODE,
};

pub(super) fn source_index(path: &DISPLAYCONFIG_PATH_INFO) -> usize {
    let index = unsafe { path.sourceInfo.Anonymous.modeInfoIdx };
    if virtual_modes(path) {
        (index >> 16) as usize
    } else {
        index as usize
    }
}

pub(super) fn virtual_modes(path: &DISPLAYCONFIG_PATH_INFO) -> bool {
    path.flags & DISPLAYCONFIG_PATH_SUPPORT_VIRTUAL_MODE != 0
}

pub(super) fn set_indices(
    path: &mut DISPLAYCONFIG_PATH_INFO,
    source: u32,
    target: u32,
    desktop: u32,
) {
    // The caller bounds the mode array below the 16-bit invalid-index sentinel.
    path.sourceInfo.Anonymous.modeInfoIdx = if virtual_modes(path) {
        (source << 16) | 0xffff // no clone group
    } else {
        source
    };
    path.targetInfo.Anonymous.modeInfoIdx = if virtual_modes(path) {
        (target << 16) | desktop
    } else {
        target
    };
}
