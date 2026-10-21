use super::{
    DpiContext, displays,
    driver_device::{Connection, DeviceHandle},
    path_modes, status_error,
};
use crate::{Display, DisplayOptions, Error, Point, Result};
use std::{
    ptr,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Devices::{DeviceAndDriverInstallation::*, Display::*},
    Foundation::{ERROR_INSUFFICIENT_BUFFER, POINTL, RECTL},
    Graphics::Gdi::DISPLAYCONFIG_PATH_ACTIVE,
};

/// An owned monitor on the installed adapter. Closing the device file removes only
/// this monitor. The adapter and driver package remain installed for later workspaces.
pub struct VirtualDisplay {
    device: Option<DeviceHandle>,
    adapter: u64,
    target: u32,
    instance: String,
    timeout: Duration,
}

impl VirtualDisplay {
    pub fn create(options: DisplayOptions) -> Result<Self> {
        options.validate()?;
        require_user_output()?;
        let connection = Connection::create(options.timeout)?;
        let mut owned = Self {
            device: Some(connection.handle),
            adapter: connection.adapter,
            target: connection.target,
            instance: connection.instance,
            timeout: options.timeout,
        };
        let deadline = Instant::now() + options.timeout;
        loop {
            if all_paths()?.0.iter().any(|path| {
                is_owned(path, owned.adapter, owned.target) && path.targetInfo.targetAvailable != 0
            }) {
                break;
            }
            if Instant::now() >= deadline {
                // IddCx arrival precedes asynchronous WDDM target enumeration.
                check_device_problem(&owned.instance)?;
                return Err(Error::Timeout("IDD monitor arrival"));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        owned.configure(options)?;
        Ok(owned)
    }

    pub fn instance_id(&self) -> &str {
        &self.instance
    }

    pub fn display(&self) -> Result<Display> {
        self.validate_lease()?;
        displays()?
            .into_iter()
            .find(|display| display.id.adapter == self.adapter && display.id.target == self.target)
            .ok_or(Error::InvalidState("owned virtual display is not active"))
    }

    /// Set resolution and placement on exactly this owned adapter. Existing paths and
    /// source modes are retained; no global SDC_TOPOLOGY_EXTEND or saved-user-layout reset.
    pub fn configure(&mut self, options: DisplayOptions) -> Result<()> {
        options.validate()?;
        self.validate_lease()?;
        let _dpi = DpiContext::enter()?;
        let existing = require_user_output()?;
        let others: Vec<_> = existing
            .iter()
            .filter(|display| {
                display.id.adapter != self.adapter || display.id.target != self.target
            })
            .collect();
        let physical: Vec<_> = others
            .iter()
            .filter(|display| !display.workspace)
            .map(|display| display.bounds)
            .collect();
        let occupied: Vec<_> = others.iter().map(|display| display.bounds).collect();
        let origin = options
            .origin
            .or_else(|| default_origin(&physical, &occupied, options.width, options.height))
            .ok_or(Error::InvalidState(
                "no free position adjacent to a user display",
            ))?;
        let right = origin
            .x
            .checked_add(options.width as i32)
            .ok_or(Error::InvalidState("display coordinate overflow"))?;
        let bottom = origin
            .y
            .checked_add(options.height as i32)
            .ok_or(Error::InvalidState("display coordinate overflow"))?;
        let requested = crate::Rect {
            left: origin.x,
            top: origin.y,
            right,
            bottom,
        };
        for display in &existing {
            if display.id.adapter == self.adapter && display.id.target == self.target {
                continue;
            }
            if requested.left < display.bounds.right
                && display.bounds.left < requested.right
                && requested.top < display.bounds.bottom
                && display.bounds.top < requested.bottom
            {
                return Err(Error::InvalidState(
                    "virtual display would overlap an existing output",
                ));
            }
        }
        let (all, modes) = all_paths()?;
        let owned = select_owned_path(&all, self.adapter, self.target)
            .ok_or(Error::InvalidState("owned IDD path disappeared"))?;
        let paths: Vec<_> = all
            .into_iter()
            .filter(|path| {
                path.flags & DISPLAYCONFIG_PATH_ACTIVE != 0
                    && !is_owned(path, self.adapter, self.target)
            })
            .collect();
        let (paths, modes) = plan_owned_output(owned, paths, modes, options, origin)?;
        // All source/target modes are explicit. Do not allow Windows to silently alter
        // another output's mode to make a best-fit configuration succeed.
        let flags = SDC_USE_SUPPLIED_DISPLAY_CONFIG | SDC_VIRTUAL_MODE_AWARE;
        self.validate_lease()?;
        let status = unsafe {
            SetDisplayConfig(
                paths.len() as u32,
                paths.as_ptr(),
                modes.len() as u32,
                modes.as_ptr(),
                flags | SDC_VALIDATE,
            )
        };
        if status != 0 {
            return Err(status_error(
                "SetDisplayConfig(validate owned IDD path)",
                status,
            ));
        }
        let status = unsafe {
            SetDisplayConfig(
                paths.len() as u32,
                paths.as_ptr(),
                modes.len() as u32,
                modes.as_ptr(),
                flags | SDC_APPLY,
            )
        };
        if status != 0 {
            return Err(status_error(
                "SetDisplayConfig(apply owned IDD path)",
                status,
            ));
        }
        self.timeout = options.timeout;
        let deadline = Instant::now() + options.timeout;
        loop {
            if self.display().is_ok_and(|display| {
                display.bounds == requested && !display.cloned && display.indirect
            }) {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(Error::Timeout("virtual display configuration"));
            }
            check_device_problem(&self.instance)?;
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    pub fn remove(mut self) -> Result<()> {
        // Never wait on an unrelated monitor that reuses an old OS target number.
        let current = self.validate_lease().and_then(|()| self.deactivate());
        drop(self.device.take());
        current?;
        let deadline = Instant::now() + self.timeout;
        loop {
            if !all_paths()?.0.iter().any(|path| {
                is_owned(path, self.adapter, self.target)
                    && (path.targetInfo.targetAvailable != 0
                        || path.flags & DISPLAYCONFIG_PATH_ACTIVE != 0)
            }) {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(Error::Timeout("virtual display removal"));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    fn deactivate(&self) -> Result<()> {
        self.validate_lease()?;
        let (paths, modes) = all_paths()?;
        if !paths.iter().any(|path| {
            is_owned(path, self.adapter, self.target) && path.flags & DISPLAYCONFIG_PATH_ACTIVE != 0
        }) {
            return Ok(());
        }
        let retained: Vec<_> = paths
            .into_iter()
            .filter(|path| {
                path.flags & DISPLAYCONFIG_PATH_ACTIVE != 0
                    && !is_owned(path, self.adapter, self.target)
            })
            .collect();
        // Disable this path before unplugging the monitor. Otherwise Windows may
        // normalize the surviving source positions while handling physical removal.
        let status = unsafe {
            SetDisplayConfig(
                retained.len() as u32,
                retained.as_ptr(),
                modes.len() as u32,
                modes.as_ptr(),
                SDC_USE_SUPPLIED_DISPLAY_CONFIG | SDC_VIRTUAL_MODE_AWARE | SDC_APPLY,
            )
        };
        if status != 0 {
            return Err(status_error(
                "SetDisplayConfig(deactivate owned IDD path)",
                status,
            ));
        }
        Ok(())
    }
    pub(super) fn validate_lease(&self) -> Result<()> {
        self.device
            .as_ref()
            .ok_or(Error::InvalidState("virtual display is closed"))?
            .validate(self.adapter, self.target)
    }
}

impl Drop for VirtualDisplay {
    fn drop(&mut self) {
        if self.device.is_some() {
            let _ = self.deactivate();
            drop(self.device.take());
        }
    }
}

fn adjacent(left: crate::Rect, right: crate::Rect) -> bool {
    ((left.right == right.left || left.left == right.right)
        && left.top < right.bottom
        && right.top < left.bottom)
        || ((left.bottom == right.top || left.top == right.bottom)
            && left.left < right.right
            && right.left < left.right)
}

fn default_origin(
    physical: &[crate::Rect],
    occupied: &[crate::Rect],
    width: u32,
    height: u32,
) -> Option<Point> {
    let width = i64::from(width);
    let height = i64::from(height);
    for screen in physical {
        let mut vertical = vec![i64::from(screen.top), i64::from(screen.bottom) - height];
        let mut horizontal = vec![i64::from(screen.left), i64::from(screen.right) - width];
        for other in occupied {
            vertical.extend([i64::from(other.bottom), i64::from(other.top) - height]);
            horizontal.extend([i64::from(other.right), i64::from(other.left) - width]);
        }
        let candidates = [i64::from(screen.right), i64::from(screen.left) - width]
            .into_iter()
            .flat_map(|x| vertical.iter().map(move |&y| (x, y)))
            .chain(
                [i64::from(screen.bottom), i64::from(screen.top) - height]
                    .into_iter()
                    .flat_map(|y| horizontal.iter().map(move |&x| (x, y))),
            );
        for (x, y) in candidates {
            let (Ok(left), Ok(top), Ok(right), Ok(bottom)) = (
                i32::try_from(x),
                i32::try_from(y),
                i32::try_from(x + width),
                i32::try_from(y + height),
            ) else {
                continue;
            };
            let requested = crate::Rect {
                left,
                top,
                right,
                bottom,
            };
            if adjacent(requested, *screen)
                && !occupied.iter().any(|other| {
                    requested.left < other.right
                        && other.left < requested.right
                        && requested.top < other.bottom
                        && other.top < requested.bottom
                })
            {
                return Some(Point { x: left, y: top });
            }
        }
    }
    None
}

#[cfg(test)]
fn to_luid(value: u64) -> windows_sys::Win32::Foundation::LUID {
    windows_sys::Win32::Foundation::LUID {
        LowPart: value as u32,
        HighPart: (value >> 32) as i32,
    }
}
fn require_user_output() -> Result<Vec<Display>> {
    let active = displays()?;
    if !active.iter().any(|display| !display.workspace) {
        return Err(Error::InvalidState("a user display must remain active"));
    }
    Ok(active)
}

fn is_owned(path: &DISPLAYCONFIG_PATH_INFO, adapter: u64, target: u32) -> bool {
    path.targetInfo.adapterId.LowPart == adapter as u32
        && path.targetInfo.adapterId.HighPart == (adapter >> 32) as i32
        && path.targetInfo.id == target
}

fn select_owned_path(
    paths: &[DISPLAYCONFIG_PATH_INFO],
    adapter: u64,
    target: u32,
) -> Option<DISPLAYCONFIG_PATH_INFO> {
    // QDC_ALL_PATHS can offer multiple sources for one target. Reuse its active
    // source, or choose a free source; never clone another workspace on this adapter.
    paths
        .iter()
        .find(|path| is_owned(path, adapter, target) && path.flags & DISPLAYCONFIG_PATH_ACTIVE != 0)
        .or_else(|| {
            paths
                .iter()
                .filter(|path| is_owned(path, adapter, target))
                .find(|path| {
                    !paths.iter().any(|other| {
                        other.flags & DISPLAYCONFIG_PATH_ACTIVE != 0
                            && !is_owned(other, adapter, target)
                            && other.sourceInfo.adapterId.LowPart
                                == path.sourceInfo.adapterId.LowPart
                            && other.sourceInfo.adapterId.HighPart
                                == path.sourceInfo.adapterId.HighPart
                            && other.sourceInfo.id == path.sourceInfo.id
                    })
                })
        })
        .copied()
}
fn check_device_problem(instance: &str) -> Result<()> {
    let mut devnode = 0;
    let instance: Vec<u16> = instance.encode_utf16().chain(Some(0)).collect();
    if unsafe { CM_Locate_DevNodeW(&mut devnode, instance.as_ptr(), CM_LOCATE_DEVNODE_NORMAL) }
        != CR_SUCCESS
    {
        return Ok(());
    }
    let (mut status, mut problem) = (0, 0);
    let result = unsafe { CM_Get_DevNode_Status(&mut status, &mut problem, devnode, 0) };
    if result == CR_SUCCESS && problem != 0 {
        return Err(Error::DeviceProblem(problem));
    }
    Ok(())
}

fn all_paths() -> Result<(Vec<DISPLAYCONFIG_PATH_INFO>, Vec<DISPLAYCONFIG_MODE_INFO>)> {
    for _ in 0..4 {
        let (mut path_count, mut mode_count) = (0, 0);
        let status = unsafe {
            GetDisplayConfigBufferSizes(
                QDC_ALL_PATHS | QDC_VIRTUAL_MODE_AWARE,
                &mut path_count,
                &mut mode_count,
            )
        };
        if status != 0 {
            return Err(status_error(
                "GetDisplayConfigBufferSizes(all)",
                status as i32,
            ));
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); path_count as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); mode_count as usize];
        let status = unsafe {
            QueryDisplayConfig(
                QDC_ALL_PATHS | QDC_VIRTUAL_MODE_AWARE,
                &mut path_count,
                paths.as_mut_ptr(),
                &mut mode_count,
                modes.as_mut_ptr(),
                ptr::null_mut(),
            )
        };
        if status == ERROR_INSUFFICIENT_BUFFER {
            continue;
        }
        if status != 0 {
            return Err(status_error("QueryDisplayConfig(all)", status as i32));
        }
        paths.truncate(path_count as usize);
        modes.truncate(mode_count as usize);
        return Ok((paths, modes));
    }
    Err(Error::InvalidState("display topology kept changing"))
}

fn plan_owned_output(
    mut owned: DISPLAYCONFIG_PATH_INFO,
    mut paths: Vec<DISPLAYCONFIG_PATH_INFO>,
    mut modes: Vec<DISPLAYCONFIG_MODE_INFO>,
    options: DisplayOptions,
    origin: Point,
) -> Result<(Vec<DISPLAYCONFIG_PATH_INFO>, Vec<DISPLAYCONFIG_MODE_INFO>)> {
    if modes.len() > 65531 {
        return Err(Error::InvalidState("too many DisplayConfig modes"));
    }
    owned.flags |= DISPLAYCONFIG_PATH_ACTIVE;
    owned.targetInfo.rotation = DISPLAYCONFIG_ROTATION_IDENTITY;
    owned.targetInfo.scanLineOrdering = DISPLAYCONFIG_SCANLINE_ORDERING_PROGRESSIVE;
    let source_index = modes.len() as u32;
    owned.targetInfo.refreshRate = DISPLAYCONFIG_RATIONAL {
        Numerator: 60,
        Denominator: 1,
    };
    modes.push(DISPLAYCONFIG_MODE_INFO {
        infoType: DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE,
        id: owned.sourceInfo.id,
        adapterId: owned.sourceInfo.adapterId,
        Anonymous: DISPLAYCONFIG_MODE_INFO_0 {
            sourceMode: DISPLAYCONFIG_SOURCE_MODE {
                width: options.width,
                height: options.height,
                pixelFormat: DISPLAYCONFIG_PIXELFORMAT_32BPP,
                position: POINTL {
                    x: origin.x,
                    y: origin.y,
                },
            },
        },
    });
    let target_index = modes.len() as u32;
    modes.push(DISPLAYCONFIG_MODE_INFO {
        infoType: DISPLAYCONFIG_MODE_INFO_TYPE_TARGET,
        id: owned.targetInfo.id,
        adapterId: owned.targetInfo.adapterId,
        Anonymous: DISPLAYCONFIG_MODE_INFO_0 {
            targetMode: DISPLAYCONFIG_TARGET_MODE {
                targetVideoSignalInfo: DISPLAYCONFIG_VIDEO_SIGNAL_INFO {
                    pixelRate: 60 * options.width as u64 * options.height as u64,
                    hSyncFreq: DISPLAYCONFIG_RATIONAL {
                        Numerator: 60 * options.height,
                        Denominator: 1,
                    },
                    vSyncFreq: DISPLAYCONFIG_RATIONAL {
                        Numerator: 60,
                        Denominator: 1,
                    },
                    activeSize: DISPLAYCONFIG_2DREGION {
                        cx: options.width,
                        cy: options.height,
                    },
                    totalSize: DISPLAYCONFIG_2DREGION {
                        cx: options.width,
                        cy: options.height,
                    },
                    // Same fixed target timings as the bundled IDD: videoStandard 255,
                    // vSyncFreqDivider 1 (bits 16..21), no interlace or blanking.
                    Anonymous: DISPLAYCONFIG_VIDEO_SIGNAL_INFO_0 {
                        videoStandard: 255 | (1 << 16),
                    },
                    scanLineOrdering: DISPLAYCONFIG_SCANLINE_ORDERING_PROGRESSIVE,
                },
            },
        },
    });
    let desktop_index = modes.len() as u32;
    if path_modes::virtual_modes(&owned) {
        let region = RECTL {
            left: 0,
            top: 0,
            right: options.width as i32,
            bottom: options.height as i32,
        };
        modes.push(DISPLAYCONFIG_MODE_INFO {
            infoType: DISPLAYCONFIG_MODE_INFO_TYPE_DESKTOP_IMAGE,
            id: owned.targetInfo.id,
            adapterId: owned.targetInfo.adapterId,
            Anonymous: DISPLAYCONFIG_MODE_INFO_0 {
                desktopImageInfo: DISPLAYCONFIG_DESKTOP_IMAGE_INFO {
                    PathSourceSize: POINTL {
                        x: options.width as i32,
                        y: options.height as i32,
                    },
                    DesktopImageRegion: region,
                    DesktopImageClip: region,
                },
            },
        });
    }
    path_modes::set_indices(&mut owned, source_index, target_index, desktop_index);
    paths.push(owned);
    Ok((paths, modes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_placement_uses_a_free_physical_edge_instead_of_chaining_workspaces() {
        let physical = crate::Rect {
            left: 0,
            top: 0,
            right: 2560,
            bottom: 1600,
        };
        let first = crate::Rect {
            left: 2560,
            top: 0,
            right: 4480,
            bottom: 1080,
        };
        let origin = default_origin(&[physical], &[physical, first], 1920, 1080).unwrap();
        assert_eq!(origin, Point { x: 2560, y: 1080 });
        let second = crate::Rect {
            left: origin.x,
            top: origin.y,
            right: origin.x + 1920,
            bottom: origin.y + 1080,
        };
        assert!(adjacent(second, physical));
        assert!(!adjacent(
            crate::Rect {
                left: 4480,
                top: 0,
                right: 6400,
                bottom: 1080
            },
            physical
        ));
    }

    #[test]
    fn second_monitor_uses_a_free_source_on_the_same_adapter() {
        let mut active = DISPLAYCONFIG_PATH_INFO {
            flags: DISPLAYCONFIG_PATH_ACTIVE,
            ..Default::default()
        };
        active.sourceInfo.adapterId = to_luid(20);
        active.targetInfo.adapterId = to_luid(20);
        active.targetInfo.outputTechnology = DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INDIRECT_WIRED;
        active.targetInfo.id = 10;
        let mut would_clone = active;
        would_clone.flags = 0;
        would_clone.targetInfo.id = 11;
        let mut independent = would_clone;
        independent.sourceInfo.id = 1;
        let selected = select_owned_path(&[active, would_clone, independent], 20, 11).unwrap();
        assert_eq!((selected.sourceInfo.id, selected.targetInfo.id), (1, 11));
        assert!(select_owned_path(&[active, would_clone], 20, 11).is_none());
        let mut already_active = independent;
        already_active.flags = DISPLAYCONFIG_PATH_ACTIVE;
        already_active.sourceInfo.id = 2;
        assert_eq!(
            select_owned_path(&[active, independent, already_active], 20, 11)
                .unwrap()
                .sourceInfo
                .id,
            2
        );
    }

    #[test]
    fn configuring_owned_output_preserves_physical_modes_and_uses_distinct_indices() {
        let mut physical = DISPLAYCONFIG_PATH_INFO {
            flags: DISPLAYCONFIG_PATH_ACTIVE,
            ..Default::default()
        };
        physical.sourceInfo.adapterId = to_luid(10);
        physical.targetInfo.adapterId = to_luid(10);
        physical.sourceInfo.Anonymous.modeInfoIdx = 0;
        physical.targetInfo.Anonymous.modeInfoIdx = 1;
        let mut source = DISPLAYCONFIG_MODE_INFO {
            infoType: DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE,
            adapterId: to_luid(10),
            ..Default::default()
        };
        source.Anonymous.sourceMode = DISPLAYCONFIG_SOURCE_MODE {
            width: 2560,
            height: 1600,
            position: POINTL { x: 0, y: 0 },
            pixelFormat: DISPLAYCONFIG_PIXELFORMAT_32BPP,
        };
        let mut target = DISPLAYCONFIG_MODE_INFO {
            infoType: DISPLAYCONFIG_MODE_INFO_TYPE_TARGET,
            adapterId: to_luid(10),
            ..Default::default()
        };
        target.Anonymous.targetMode.targetVideoSignalInfo.pixelRate = 241_500_000;
        let mut owned = DISPLAYCONFIG_PATH_INFO::default();
        owned.sourceInfo.adapterId = to_luid(20);
        owned.sourceInfo.id = 55;
        owned.targetInfo.adapterId = to_luid(20);
        owned.targetInfo.id = 7;
        let (paths, modes) = plan_owned_output(
            owned,
            vec![physical],
            vec![source, target],
            DisplayOptions {
                width: 1280,
                height: 720,
                ..Default::default()
            },
            Point { x: -1280, y: 0 },
        )
        .unwrap();
        // Existing physical modes keep both their contents and their original references.
        assert_eq!(unsafe { paths[0].sourceInfo.Anonymous.modeInfoIdx }, 0);
        assert_eq!(unsafe { paths[0].targetInfo.Anonymous.modeInfoIdx }, 1);
        let original = unsafe { modes[0].Anonymous.sourceMode };
        assert_eq!(
            (original.width, original.height, original.position.x),
            (2560, 1600, 0)
        );
        assert_eq!(
            unsafe {
                modes[1]
                    .Anonymous
                    .targetMode
                    .targetVideoSignalInfo
                    .pixelRate
            },
            241_500_000
        );
        // The owned adapter gets new, separate source and target modes, with its IDs.
        assert_eq!(unsafe { paths[1].sourceInfo.Anonymous.modeInfoIdx }, 2);
        assert_eq!(unsafe { paths[1].targetInfo.Anonymous.modeInfoIdx }, 3);
        assert_eq!((modes[2].id, modes[3].id), (55, 7));
        let virtual_source = unsafe { modes[2].Anonymous.sourceMode };
        assert_eq!(
            (
                virtual_source.width,
                virtual_source.height,
                virtual_source.position.x
            ),
            (1280, 720, -1280)
        );
        assert_eq!(
            unsafe {
                modes[3]
                    .Anonymous
                    .targetMode
                    .targetVideoSignalInfo
                    .pixelRate
            },
            55_296_000
        );
    }

    #[test]
    fn virtual_aware_paths_pack_mode_indices_and_include_the_desktop_image() {
        use windows_sys::Win32::Graphics::Gdi::DISPLAYCONFIG_PATH_SUPPORT_VIRTUAL_MODE;
        let mut owned = DISPLAYCONFIG_PATH_INFO {
            flags: DISPLAYCONFIG_PATH_SUPPORT_VIRTUAL_MODE,
            ..Default::default()
        };
        owned.sourceInfo.adapterId = to_luid(20);
        owned.sourceInfo.id = 5;
        owned.targetInfo.adapterId = to_luid(20);
        owned.targetInfo.id = 256;
        let (paths, modes) = plan_owned_output(
            owned,
            vec![],
            vec![],
            DisplayOptions {
                width: 1600,
                height: 900,
                ..Default::default()
            },
            Point { x: -1600, y: 0 },
        )
        .unwrap();
        let path = &paths[0];
        // Windows' virtual-aware wire layout: low 16 bits are clone/desktop,
        // high 16 bits are source/target. 0xffff is an invalid clone group.
        assert_eq!(unsafe { path.sourceInfo.Anonymous.modeInfoIdx }, 0x0000ffff);
        assert_eq!(unsafe { path.targetInfo.Anonymous.modeInfoIdx }, 0x00010002);
        assert_eq!(path_modes::source_index(path), 0);
        assert_eq!(
            modes[2].infoType,
            DISPLAYCONFIG_MODE_INFO_TYPE_DESKTOP_IMAGE
        );
        let image = unsafe { modes[2].Anonymous.desktopImageInfo };
        assert_eq!(
            (image.PathSourceSize.x, image.PathSourceSize.y),
            (1600, 900)
        );
        assert_eq!(
            (
                image.DesktopImageRegion.left,
                image.DesktopImageRegion.right
            ),
            (0, 1600)
        );
        assert_eq!(
            (image.DesktopImageClip.top, image.DesktopImageClip.bottom),
            (0, 900)
        );
        let mut observed = *path;
        observed.sourceInfo.Anonymous.modeInfoIdx = 0x0002ffff;
        assert_eq!(path_modes::source_index(&observed), 2);
    }
}
