//! Fixed 60 Hz modes. Timings match the host's supplied DisplayConfig target modes.
use crate::ffi::*;
use std::mem::size_of;
const MODES: &[(u32, u32)] = &[
    (1280, 720),
    (1600, 900),
    (1920, 1080),
    (2560, 1440),
    (3840, 2160),
];

fn signal(width: u32, height: u32, target: bool) -> DISPLAYCONFIG_VIDEO_SIGNAL_INFO {
    let mut value = DISPLAYCONFIG_VIDEO_SIGNAL_INFO {
        pixelRate: 60 * u64::from(width) * u64::from(height),
        hSyncFreq: DISPLAYCONFIG_RATIONAL {
            Numerator: 60 * height,
            Denominator: 1,
        },
        vSyncFreq: DISPLAYCONFIG_RATIONAL {
            Numerator: 60,
            Denominator: 1,
        },
        activeSize: DISPLAYCONFIG_2DREGION {
            cx: width,
            cy: height,
        },
        totalSize: DISPLAYCONFIG_2DREGION {
            cx: width,
            cy: height,
        },
        scanLineOrdering:
            DISPLAYCONFIG_SCANLINE_ORDERING_DISPLAYCONFIG_SCANLINE_ORDERING_PROGRESSIVE,
        ..Default::default()
    };
    value.__bindgen_anon_1.videoStandard = 255 | if target { 1 << 16 } else { 0 };
    value
}

pub unsafe extern "C" fn description(
    _: IDDCX_MONITOR,
    input: *const IDARG_IN_GETDEFAULTDESCRIPTIONMODES,
    output: *mut IDARG_OUT_GETDEFAULTDESCRIPTIONMODES,
) -> NTSTATUS {
    crate::trace::status("default description modes", unsafe {
        (*input).DefaultMonitorModeBufferInputCount
    } as i32);
    unsafe {
        (*output).DefaultMonitorModeBufferOutputCount = MODES.len() as u32;
        (*output).PreferredMonitorModeIdx = 2;
        if (*input).DefaultMonitorModeBufferInputCount == 0 {
            return 0;
        }
        if ((*input).DefaultMonitorModeBufferInputCount as usize) < MODES.len() {
            return 0xc0000023u32 as i32;
        }
        for (index, &(width, height)) in MODES.iter().enumerate() {
            *(*input).pDefaultMonitorModes.add(index) = IDDCX_MONITOR_MODE {
                Size: size_of::<IDDCX_MONITOR_MODE>() as u32,
                Origin: IDDCX_MONITOR_MODE_ORIGIN_IDDCX_MONITOR_MODE_ORIGIN_DRIVER,
                MonitorVideoSignalInfo: signal(width, height, false),
            };
        }
    }
    0
}
pub unsafe extern "C" fn target(
    _: IDDCX_MONITOR,
    input: *const IDARG_IN_QUERYTARGETMODES,
    output: *mut IDARG_OUT_QUERYTARGETMODES,
) -> NTSTATUS {
    crate::trace::status(
        "target modes",
        unsafe { (*input).TargetModeBufferInputCount } as i32,
    );
    unsafe {
        (*output).TargetModeBufferOutputCount = MODES.len() as u32;
        if (*input).TargetModeBufferInputCount == 0 {
            return 0;
        }
        if ((*input).TargetModeBufferInputCount as usize) < MODES.len() {
            return 0xc0000023u32 as i32;
        }
        for (index, &(width, height)) in MODES.iter().enumerate() {
            let mut mode = IDDCX_TARGET_MODE {
                Size: size_of::<IDDCX_TARGET_MODE>() as u32,
                ..Default::default()
            };
            mode.TargetVideoSignalInfo.targetVideoSignalInfo = signal(width, height, true);
            *(*input).pTargetModes.add(index) = mode;
        }
    }
    0
}
pub unsafe extern "C" fn parse(
    _: *const IDARG_IN_PARSEMONITORDESCRIPTION,
    _: *mut IDARG_OUT_PARSEMONITORDESCRIPTION,
) -> NTSTATUS {
    crate::trace::status("parse description", 0);
    0xc00000bbu32 as i32
}
pub unsafe extern "C" fn commit(_: IDDCX_ADAPTER, _: *const IDARG_IN_COMMITMODES) -> NTSTATUS {
    crate::trace::status("commit modes", 0);
    0
}
