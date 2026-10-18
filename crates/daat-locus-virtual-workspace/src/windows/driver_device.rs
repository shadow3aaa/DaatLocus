use super::{last_error, status_error};
use crate::{Error, Result, driver_protocol as protocol};
use std::{
    ptr,
    time::{Duration, Instant},
};
use uuid::Uuid;
use windows_sys::{
    Win32::{
        Devices::{
            DeviceAndDriverInstallation::*,
            Properties::{DEVPKEY_Device_InstanceId, DEVPROP_TYPE_STRING},
        },
        Foundation::{
            CloseHandle, ERROR_NOT_READY, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
        },
        Storage::FileSystem::{
            CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
        },
        System::IO::DeviceIoControl,
    },
    core::GUID,
};

pub(super) struct DeviceHandle(HANDLE);
impl DeviceHandle {
    pub fn validate(&self, adapter: u64, target: u32) -> Result<()> {
        let input = protocol::request([0; 16]);
        let mut output = [0; protocol::RESPONSE_BYTES];
        let mut returned = 0;
        if unsafe {
            DeviceIoControl(
                self.0,
                protocol::QUERY,
                input.as_ptr().cast(),
                input.len() as u32,
                output.as_mut_ptr().cast(),
                output.len() as u32,
                &mut returned,
                ptr::null_mut(),
            )
        } == 0
        {
            return Err(last_error("validate owned IDD monitor lease"));
        }
        if returned as usize != output.len()
            || protocol::parse_response(&output) != Some((adapter, target))
        {
            return Err(Error::InvalidState(
                "owned IDD monitor lease changed; create a new workspace",
            ));
        }
        Ok(())
    }
}
impl Drop for DeviceHandle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

pub(super) struct Connection {
    pub handle: DeviceHandle,
    pub instance: String,
    pub adapter: u64,
    pub target: u32,
}
impl Connection {
    pub fn create(timeout: Duration) -> Result<Self> {
        let guid = GUID::from_u128(protocol::INTERFACE_GUID);
        let mut length = 0;
        let status = unsafe {
            CM_Get_Device_Interface_List_SizeW(
                &mut length,
                &guid,
                ptr::null(),
                CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
            )
        };
        if status != CR_SUCCESS {
            return Err(status_error(
                "enumerate Daat Locus driver interface",
                unsafe { CM_MapCrToWin32Err(status, 31) } as i32,
            ));
        }
        if length <= 1 || length > 32768 {
            return Err(Error::InvalidState(
                "Daat Locus display driver is not installed or not running; install its signed package first",
            ));
        }
        let mut paths = vec![0u16; length as usize];
        let status = unsafe {
            CM_Get_Device_Interface_ListW(
                &guid,
                ptr::null(),
                paths.as_mut_ptr(),
                length,
                CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
            )
        };
        if status != CR_SUCCESS {
            return Err(status_error("read Daat Locus driver interface", unsafe {
                CM_MapCrToWin32Err(status, 31)
            }
                as i32));
        }
        let interfaces: Vec<_> = paths
            .split(|unit| *unit == 0)
            .filter(|path| !path.is_empty())
            .collect();
        if interfaces.len() != 1 {
            return Err(Error::InvalidState(
                "expected exactly one installed Daat Locus adapter",
            ));
        }
        let mut path = interfaces[0].to_vec();
        path.push(0);
        let mut instance = [0u16; 512];
        let mut bytes = size_of_val(&instance) as u32;
        let mut kind = 0;
        let status = unsafe {
            CM_Get_Device_Interface_PropertyW(
                path.as_ptr(),
                &DEVPKEY_Device_InstanceId,
                &mut kind,
                instance.as_mut_ptr().cast(),
                &mut bytes,
                0,
            )
        };
        if status != CR_SUCCESS
            || kind != DEVPROP_TYPE_STRING
            || bytes as usize > size_of_val(&instance)
        {
            return Err(Error::InvalidState(
                "cannot identify the installed Daat Locus display adapter",
            ));
        }
        let end = instance
            .iter()
            .position(|unit| *unit == 0)
            .ok_or(Error::InvalidState("unterminated adapter identity"))?;
        let instance = String::from_utf16_lossy(&instance[..end]);
        let handle = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                ptr::null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(last_error("open Daat Locus display driver"));
        }
        let handle = DeviceHandle(handle);
        let input = protocol::request(*Uuid::new_v4().as_bytes());
        let mut output = [0; protocol::RESPONSE_BYTES];
        let deadline = Instant::now() + timeout;
        loop {
            let mut returned = 0;
            if unsafe {
                DeviceIoControl(
                    handle.0,
                    protocol::CREATE,
                    input.as_ptr().cast(),
                    input.len() as u32,
                    output.as_mut_ptr().cast(),
                    output.len() as u32,
                    &mut returned,
                    ptr::null_mut(),
                )
            } != 0
            {
                if returned as usize != output.len() {
                    return Err(Error::InvalidState(
                        "incompatible Daat Locus display driver response length",
                    ));
                }
                let (adapter, target) = protocol::parse_response(&output).ok_or(
                    Error::InvalidState("incompatible Daat Locus display driver response"),
                )?;
                return Ok(Self {
                    handle,
                    instance,
                    adapter,
                    target,
                });
            }
            let error = std::io::Error::last_os_error();
            // NOT_READY guarantees the driver has created no monitor. Never retry an
            // uncertain effect after cancellation, device loss, or any other failure.
            if error.raw_os_error() != Some(ERROR_NOT_READY as i32) {
                return Err(Error::Windows {
                    operation: "create owned IDD monitor",
                    source: error,
                });
            }
            if Instant::now() >= deadline {
                return Err(Error::Timeout("IDD adapter initialization"));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}
