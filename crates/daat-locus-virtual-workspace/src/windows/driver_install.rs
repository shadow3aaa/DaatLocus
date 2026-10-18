//! Conventional PnP package installation. Normal Windows elevation/publisher prompts
//! are preserved; no registry editor, certificate import, or signing-policy changes.
use super::last_error;
use crate::{Error, Result};
use std::{
    mem::size_of,
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    ptr,
};
use windows_sys::{
    Win32::{
        Devices::DeviceAndDriverInstallation::*,
        Foundation::{ERROR_NO_MORE_ITEMS, GetLastError},
    },
    core::{GUID, w},
};

const DISPLAY_CLASS: GUID = GUID::from_u128(0x4d36e968_e325_11ce_bfc1_08002be10318);
const HARDWARE_ID: &str = "Root\\DaatLocusVirtualDisplay";

pub struct DriverPackage {
    pub(super) directory: PathBuf,
}
impl DriverPackage {
    pub fn open(directory: impl AsRef<Path>) -> Result<Self> {
        let directory = directory
            .as_ref()
            .canonicalize()
            .map_err(|source| Error::Windows {
                operation: "open display driver package",
                source,
            })?;
        for file in [
            "DaatLocusVirtualDisplay.inf",
            "DaatLocusVirtualDisplay.dll",
            "DaatLocusVirtualDisplay.cat",
        ] {
            if !directory.join(file).is_file() {
                return Err(Error::InvalidState(
                    "display driver package requires its INF, DLL and signed catalog",
                ));
            }
        }
        Ok(Self { directory })
    }

    /// Requires OS administrator privileges; Windows may ask to trust the publisher.
    /// Creates the adapter once, with no monitors until a workspace opens its interface.
    /// true indicates a required reboot. Never changes root/publisher trust stores.
    pub fn install(&self) -> Result<bool> {
        let inf: Vec<u16> = self
            .directory
            .join("DaatLocusVirtualDisplay.inf")
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect();
        let mut reboot = 0;
        // A fresh machine has no matching root-enumerated device yet. Register it
        // before asking newdev to install/bind; otherwise it can return NO_MORE_ITEMS.
        // The guard removes only a newly registered node if deployment fails.
        let mut adapter = InstalledAdapter::find_or_register()?;
        if unsafe { DiInstallDriverW(ptr::null_mut(), inf.as_ptr(), 0, &mut reboot) } == 0 {
            // newdev also returns this when the installed version already matches.
            // Still run the normal bind step; never force a downgrade or suppress errors.
            if unsafe { GetLastError() } != ERROR_NO_MORE_ITEMS {
                return Err(last_error("DiInstallDriverW(DaatLocusVirtualDisplay)"));
            }
        }
        let hardware: Vec<u16> = HARDWARE_ID.encode_utf16().chain(Some(0)).collect();
        let mut binding_reboot = 0;
        if unsafe {
            UpdateDriverForPlugAndPlayDevicesW(
                ptr::null_mut(),
                hardware.as_ptr(),
                inf.as_ptr(),
                0,
                &mut binding_reboot,
            )
        } == 0
        {
            return Err(last_error(
                "UpdateDriverForPlugAndPlayDevicesW(DaatLocusVirtualDisplay)",
            ));
        }
        adapter.newly_registered = false;
        Ok(reboot != 0 || binding_reboot != 0)
    }

    #[cfg(feature = "driver-setup")]
    pub(super) fn uninstall(&self) -> Result<bool> {
        let inf: Vec<u16> = self
            .directory
            .join("DaatLocusVirtualDisplay.inf")
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect();
        let mut reboot = 0;
        if unsafe { DiUninstallDriverW(ptr::null_mut(), inf.as_ptr(), 0, &mut reboot) } == 0 {
            return Err(last_error("DiUninstallDriverW(DaatLocusVirtualDisplay)"));
        }
        Ok(reboot != 0)
    }
}

#[cfg(feature = "driver-setup")]
pub(super) fn remove_driver_adapters() -> Result<bool> {
    // Include phantom nodes, but only match our exact hardware ID.
    let handle = unsafe { SetupDiGetClassDevsW(&DISPLAY_CLASS, ptr::null(), ptr::null_mut(), 0) };
    if handle == -1 {
        return Err(last_error("enumerate display adapters for removal"));
    }
    let set = DeviceSet(handle);
    let mut adapters = Vec::new();
    for index in 0..4096 {
        let mut info = SP_DEVINFO_DATA {
            cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
            ..Default::default()
        };
        if unsafe { SetupDiEnumDeviceInfo(set.0, index, &mut info) } == 0 {
            if unsafe { GetLastError() } == ERROR_NO_MORE_ITEMS {
                break;
            }
            return Err(last_error("enumerate display adapter for removal"));
        }
        if is_our_adapter(&set, &info) {
            adapters.push(info);
        }
    }
    let mut reboot = false;
    for info in adapters {
        let mut required = 0;
        if unsafe { DiUninstallDevice(ptr::null_mut(), set.0, &info, 0, &mut required) } == 0 {
            return Err(last_error("remove Daat Locus display adapter"));
        }
        reboot |= required != 0;
    }
    Ok(reboot)
}

fn is_our_adapter(set: &DeviceSet, info: &SP_DEVINFO_DATA) -> bool {
    let mut ids = [0u16; 1024];
    let mut kind = 0;
    (unsafe {
        SetupDiGetDeviceRegistryPropertyW(
            set.0,
            info,
            SPDRP_HARDWAREID,
            &mut kind,
            ids.as_mut_ptr().cast(),
            size_of_val(&ids) as u32,
            ptr::null_mut(),
        )
    }) != 0
        && ids
            .split(|unit| *unit == 0)
            .any(|id| String::from_utf16_lossy(id).eq_ignore_ascii_case(HARDWARE_ID))
}

struct DeviceSet(HDEVINFO);
impl Drop for DeviceSet {
    fn drop(&mut self) {
        unsafe {
            SetupDiDestroyDeviceInfoList(self.0);
        }
    }
}
struct InstalledAdapter {
    set: DeviceSet,
    info: SP_DEVINFO_DATA,
    newly_registered: bool,
}
impl InstalledAdapter {
    fn find_or_register() -> Result<Self> {
        let handle = unsafe {
            SetupDiGetClassDevsW(&DISPLAY_CLASS, ptr::null(), ptr::null_mut(), DIGCF_PRESENT)
        };
        if handle == -1 {
            return Err(last_error("SetupDiGetClassDevsW(display)"));
        }
        let set = DeviceSet(handle);
        for index in 0..4096 {
            let mut info = SP_DEVINFO_DATA {
                cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
                ..Default::default()
            };
            if unsafe { SetupDiEnumDeviceInfo(set.0, index, &mut info) } == 0 {
                if unsafe { GetLastError() } == ERROR_NO_MORE_ITEMS {
                    break;
                }
                return Err(last_error("SetupDiEnumDeviceInfo(display)"));
            }
            if is_our_adapter(&set, &info) {
                return Ok(Self {
                    set,
                    info,
                    newly_registered: false,
                });
            }
        }
        drop(set);
        let handle = unsafe { SetupDiCreateDeviceInfoList(&DISPLAY_CLASS, ptr::null_mut()) };
        if handle == -1 {
            return Err(last_error("SetupDiCreateDeviceInfoList(display)"));
        }
        let set = DeviceSet(handle);
        let mut info = SP_DEVINFO_DATA {
            cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
            ..Default::default()
        };
        if unsafe {
            SetupDiCreateDeviceInfoW(
                set.0,
                w!("Display"),
                &DISPLAY_CLASS,
                w!("Daat Locus Virtual Workspace Display"),
                ptr::null_mut(),
                DICD_GENERATE_ID,
                &mut info,
            )
        } == 0
        {
            return Err(last_error("SetupDiCreateDeviceInfoW(DaatLocus)"));
        }
        let hardware: Vec<u16> = HARDWARE_ID.encode_utf16().chain([0, 0]).collect();
        if unsafe {
            SetupDiSetDeviceRegistryPropertyW(
                set.0,
                &mut info,
                SPDRP_HARDWAREID,
                hardware.as_ptr().cast(),
                (hardware.len() * 2) as u32,
            )
        } == 0
        {
            return Err(last_error("SetupDiSetDeviceRegistryPropertyW(hardware ID)"));
        }
        if unsafe { SetupDiCallClassInstaller(DIF_REGISTERDEVICE, set.0, &info) } == 0 {
            return Err(last_error(
                "SetupDiCallClassInstaller(register display adapter)",
            ));
        }
        Ok(Self {
            set,
            info,
            newly_registered: true,
        })
    }
}
impl Drop for InstalledAdapter {
    fn drop(&mut self) {
        if self.newly_registered {
            // Roll back only our just-created node if binding failed; keep preexisting devices.
            unsafe {
                SetupDiCallClassInstaller(DIF_REMOVE, self.set.0, &self.info);
            }
        }
    }
}
