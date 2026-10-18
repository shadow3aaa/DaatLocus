use super::{DpiContext, path_modes::source_index, status_error};
use crate::{Display, DisplayId, Error, Rect, Result};
use std::mem::size_of;
use windows_sys::Win32::{
    Devices::Display::*,
    Foundation::{ERROR_INSUFFICIENT_BUFFER, LUID},
};

fn luid(value: LUID) -> u64 {
    ((value.HighPart as u32 as u64) << 32) | value.LowPart as u64
}
fn text(value: &[u16]) -> String {
    String::from_utf16_lossy(&value[..value.iter().position(|c| *c == 0).unwrap_or(value.len())])
}

pub fn displays() -> Result<Vec<Display>> {
    let _dpi = DpiContext::enter()?;
    // Topology can change between sizing and querying. Retry only this read-only race.
    for _ in 0..4 {
        let (mut path_count, mut mode_count) = (0, 0);
        let status = unsafe {
            GetDisplayConfigBufferSizes(
                QDC_ONLY_ACTIVE_PATHS | QDC_VIRTUAL_MODE_AWARE,
                &mut path_count,
                &mut mode_count,
            )
        };
        if status != 0 {
            return Err(status_error("GetDisplayConfigBufferSizes", status as i32));
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); path_count as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); mode_count as usize];
        let status = unsafe {
            QueryDisplayConfig(
                QDC_ONLY_ACTIVE_PATHS | QDC_VIRTUAL_MODE_AWARE,
                &mut path_count,
                paths.as_mut_ptr(),
                &mut mode_count,
                modes.as_mut_ptr(),
                std::ptr::null_mut(),
            )
        };
        if status == ERROR_INSUFFICIENT_BUFFER {
            continue;
        }
        if status != 0 {
            return Err(status_error("QueryDisplayConfig", status as i32));
        }
        paths.truncate(path_count as usize);
        modes.truncate(mode_count as usize);
        return paths
            .iter()
            .map(|path| {
                let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
                source.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
                source.header.size = size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;
                source.header.adapterId = path.sourceInfo.adapterId;
                source.header.id = path.sourceInfo.id;
                let status = unsafe { DisplayConfigGetDeviceInfo(&mut source.header) };
                if status != 0 {
                    return Err(status_error("DisplayConfigGetDeviceInfo(source)", status));
                }
                let mut target = DISPLAYCONFIG_TARGET_DEVICE_NAME::default();
                target.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME;
                target.header.size = size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32;
                target.header.adapterId = path.targetInfo.adapterId;
                target.header.id = path.targetInfo.id;
                let status = unsafe { DisplayConfigGetDeviceInfo(&mut target.header) };
                if status != 0 {
                    return Err(status_error("DisplayConfigGetDeviceInfo(target)", status));
                }
                let mode = modes
                    .get(source_index(path))
                    .filter(|m| {
                        m.infoType == DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE
                            && m.id == path.sourceInfo.id
                            && luid(m.adapterId) == luid(path.sourceInfo.adapterId)
                    })
                    .ok_or(Error::InvalidState("display has no matching source mode"))?;
                let mode = unsafe { mode.Anonymous.sourceMode };
                let right = i32::try_from(mode.width)
                    .ok()
                    .and_then(|w| mode.position.x.checked_add(w))
                    .ok_or(Error::InvalidState("display width overflow"))?;
                let bottom = i32::try_from(mode.height)
                    .ok()
                    .and_then(|h| mode.position.y.checked_add(h))
                    .ok_or(Error::InvalidState("display height overflow"))?;
                Ok(Display {
                    id: DisplayId {
                        adapter: luid(path.targetInfo.adapterId),
                        target: path.targetInfo.id,
                        monitor_path: text(&target.monitorDevicePath),
                    },
                    name: text(&target.monitorFriendlyDeviceName),
                    device_name: text(&source.viewGdiDeviceName),
                    bounds: Rect {
                        left: mode.position.x,
                        top: mode.position.y,
                        right,
                        bottom,
                    },
                    rotation: path.targetInfo.rotation,
                    output_technology: path.targetInfo.outputTechnology,
                    indirect: is_indirect(path)?,
                    cloned: paths
                        .iter()
                        .filter(|p| {
                            luid(p.sourceInfo.adapterId) == luid(path.sourceInfo.adapterId)
                                && p.sourceInfo.id == path.sourceInfo.id
                        })
                        .count()
                        != 1,
                })
            })
            .collect();
    }
    Err(Error::InvalidState(
        "display topology kept changing during enumeration",
    ))
}

fn is_indirect(path: &DISPLAYCONFIG_PATH_INFO) -> Result<bool> {
    let mut base = DISPLAYCONFIG_TARGET_BASE_TYPE::default();
    base.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_BASE_TYPE;
    base.header.size = size_of::<DISPLAYCONFIG_TARGET_BASE_TYPE>() as u32;
    base.header.adapterId = path.targetInfo.adapterId;
    base.header.id = path.targetInfo.id;
    let status = unsafe { DisplayConfigGetDeviceInfo(&mut base.header) };
    if status != 0 {
        return Err(status_error(
            "DisplayConfigGetDeviceInfo(base technology)",
            status,
        ));
    }
    Ok(matches!(
        base.baseOutputTechnology,
        DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INDIRECT_VIRTUAL
            | DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INDIRECT_WIRED
    ))
}
