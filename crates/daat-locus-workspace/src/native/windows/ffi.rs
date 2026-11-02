// Dokan 2 ABI types adapted from dokan-sys 0.3.1+dokan206 (MIT).
// Copyright (c) 2020 DDoSolitary. See third-party/dokan-rust-LICENSE.
// Functions are loaded dynamically from the installed system DLL; a missing
// driver must never prevent the service executable from starting.
#![allow(non_camel_case_types, non_snake_case, dead_code)]
use std::ffi::c_int;

use winapi::{
    shared::{
        basetsd::ULONG64,
        minwindef::{BOOL, DWORD, FILETIME, LPCVOID, LPDWORD, LPVOID, MAX_PATH},
        ntdef::{
            BOOLEAN, LONGLONG, LPCWSTR, LPWSTR, NTSTATUS, PULONG, PULONGLONG, PVOID, SCHAR, UCHAR,
            ULONG, UNICODE_STRING, USHORT, WCHAR,
        },
    },
    um::{
        fileapi::LPBY_HANDLE_FILE_INFORMATION,
        minwinbase::PWIN32_FIND_DATAW,
        winnt::{ACCESS_MASK, PSECURITY_DESCRIPTOR, PSECURITY_INFORMATION},
    },
};

#[repr(C)]
pub struct WIN32_FIND_STREAM_DATA {
    pub StreamSize: winapi::shared::ntdef::LARGE_INTEGER,
    pub cStreamName: [WCHAR; MAX_PATH + 36],
}
pub type PWIN32_FIND_STREAM_DATA = *mut WIN32_FIND_STREAM_DATA;
pub const DOKAN_OPTION_DEBUG: ULONG = 1 << 0;
pub const DOKAN_OPTION_STDERR: ULONG = 1 << 1;
pub const DOKAN_OPTION_ALT_STREAM: ULONG = 1 << 2;
pub const DOKAN_OPTION_WRITE_PROTECT: ULONG = 1 << 3;
pub const DOKAN_OPTION_NETWORK: ULONG = 1 << 4;
pub const DOKAN_OPTION_REMOVABLE: ULONG = 1 << 5;
pub const DOKAN_OPTION_MOUNT_MANAGER: ULONG = 1 << 6;
pub const DOKAN_OPTION_CURRENT_SESSION: ULONG = 1 << 7;
pub const DOKAN_OPTION_FILELOCK_USER_MODE: ULONG = 1 << 8;
pub const DOKAN_OPTION_CASE_SENSITIVE: ULONG = 1 << 9;
pub const DOKAN_OPTION_ENABLE_UNMOUNT_NETWORK_DRIVE: ULONG = 1 << 10;
pub const DOKAN_OPTION_DISPATCH_DRIVER_LOGS: ULONG = 1 << 11;
pub const DOKAN_OPTION_ALLOW_IPC_BATCHING: ULONG = 1 << 12;

pub type DOKAN_HANDLE = *mut std::ffi::c_void;
pub type PDOKAN_HANDLE = *mut DOKAN_HANDLE;

pub const VOLUME_SECURITY_DESCRIPTOR_MAX_SIZE: usize = 1024 * 16;

#[repr(C)]
#[derive(Debug)]
pub struct DOKAN_OPTIONS {
    pub Version: USHORT,
    pub SingleThread: BOOLEAN,
    pub Options: ULONG,
    pub GlobalContext: ULONG64,
    pub MountPoint: LPCWSTR,
    pub UNCName: LPCWSTR,
    pub Timeout: ULONG,
    pub AllocationUnitSize: ULONG,
    pub SectorSize: ULONG,
    pub VolumeSecurityDescriptorLength: ULONG,
    pub VolumeSecurityDescriptor: [SCHAR; VOLUME_SECURITY_DESCRIPTOR_MAX_SIZE],
}

pub type PDOKAN_OPTIONS = *mut DOKAN_OPTIONS;

#[repr(C)]
#[derive(Debug)]
pub struct DOKAN_FILE_INFO {
    pub Context: ULONG64,
    pub DokanContext: ULONG64,
    pub DokanOptions: PDOKAN_OPTIONS,
    pub ProcessingContext: PVOID,
    pub ProcessId: ULONG,
    pub IsDirectory: UCHAR,
    pub DeleteOnClose: UCHAR,
    pub PagingIo: UCHAR,
    pub SynchronousIo: UCHAR,
    pub Nocache: UCHAR,
    pub WriteToEndOfFile: UCHAR,
}

pub type PDOKAN_FILE_INFO = *mut DOKAN_FILE_INFO;

pub type PFillFindData = unsafe extern "system" fn(PWIN32_FIND_DATAW, PDOKAN_FILE_INFO) -> c_int;
pub type PFillFindStreamData = unsafe extern "system" fn(PWIN32_FIND_STREAM_DATA, PVOID) -> BOOL;

#[repr(C)]
pub struct DOKAN_ACCESS_STATE {
    pub SecurityEvaluated: BOOLEAN,
    pub GenerateAudit: BOOLEAN,
    pub GenerateOnClose: BOOLEAN,
    pub AuditPrivileges: BOOLEAN,
    pub Flags: ULONG,
    pub RemainingDesiredAccess: ACCESS_MASK,
    pub PreviouslyGrantedAccess: ACCESS_MASK,
    pub OriginalDesiredAccess: ACCESS_MASK,
    pub SecurityDescriptor: PSECURITY_DESCRIPTOR,
    pub ObjectName: UNICODE_STRING,
    pub ObjectType: UNICODE_STRING,
}

pub type PDOKAN_ACCESS_STATE = *mut DOKAN_ACCESS_STATE;

#[repr(C)]
pub struct DOKAN_IO_SECURITY_CONTEXT {
    pub AccessState: DOKAN_ACCESS_STATE,
    pub DesiredAccess: ACCESS_MASK,
}

pub type PDOKAN_IO_SECURITY_CONTEXT = *mut DOKAN_IO_SECURITY_CONTEXT;

#[repr(C)]
#[derive(Clone)]
pub struct DOKAN_OPERATIONS {
    pub ZwCreateFile: Option<
        extern "system" fn(
            FileName: LPCWSTR,
            SecurityContext: PDOKAN_IO_SECURITY_CONTEXT,
            DesiredAccess: ACCESS_MASK,
            FileAttributes: ULONG,
            ShareAccess: ULONG,
            CreateDisposition: ULONG,
            CreateOptions: ULONG,
            DokanFileInfo: PDOKAN_FILE_INFO,
        ) -> NTSTATUS,
    >,
    pub Cleanup: Option<extern "system" fn(FileName: LPCWSTR, DokanFileInfo: PDOKAN_FILE_INFO)>,
    pub CloseFile: Option<extern "system" fn(FileName: LPCWSTR, DokanFileInfo: PDOKAN_FILE_INFO)>,
    pub ReadFile: Option<
        extern "system" fn(
            FileName: LPCWSTR,
            Buffer: LPVOID,
            BufferLength: DWORD,
            ReadLength: LPDWORD,
            Offset: LONGLONG,
            DokanFileInfo: PDOKAN_FILE_INFO,
        ) -> NTSTATUS,
    >,
    pub WriteFile: Option<
        extern "system" fn(
            FileName: LPCWSTR,
            Buffer: LPCVOID,
            NumberOfBytesToWrite: DWORD,
            NumberOfBytesWritten: LPDWORD,
            Offset: LONGLONG,
            DokanFileInfo: PDOKAN_FILE_INFO,
        ) -> NTSTATUS,
    >,
    pub FlushFileBuffers:
        Option<extern "system" fn(FileName: LPCWSTR, DokanFileInfo: PDOKAN_FILE_INFO) -> NTSTATUS>,
    pub GetFileInformation: Option<
        extern "system" fn(
            FileName: LPCWSTR,
            Buffer: LPBY_HANDLE_FILE_INFORMATION,
            DokanFileInfo: PDOKAN_FILE_INFO,
        ) -> NTSTATUS,
    >,
    pub FindFiles: Option<
        extern "system" fn(
            FileName: LPCWSTR,
            FillFindData: PFillFindData,
            DokanFileInfo: PDOKAN_FILE_INFO,
        ) -> NTSTATUS,
    >,
    pub FindFilesWithPattern: Option<
        extern "system" fn(
            PathName: LPCWSTR,
            SearchPattern: LPCWSTR,
            FillFindData: PFillFindData,
            DokanFileInfo: PDOKAN_FILE_INFO,
        ) -> NTSTATUS,
    >,
    pub SetFileAttributes: Option<
        extern "system" fn(
            FileName: LPCWSTR,
            FileAttributes: DWORD,
            DokanFileInfo: PDOKAN_FILE_INFO,
        ) -> NTSTATUS,
    >,
    pub SetFileTime: Option<
        extern "system" fn(
            FileName: LPCWSTR,
            creation_time: *const FILETIME,
            last_access_time: *const FILETIME,
            last_write_time: *const FILETIME,
            DokanFileInfo: PDOKAN_FILE_INFO,
        ) -> NTSTATUS,
    >,
    pub DeleteFile:
        Option<extern "system" fn(FileName: LPCWSTR, DokanFileInfo: PDOKAN_FILE_INFO) -> NTSTATUS>,
    pub DeleteDirectory:
        Option<extern "system" fn(FileName: LPCWSTR, DokanFileInfo: PDOKAN_FILE_INFO) -> NTSTATUS>,
    pub MoveFile: Option<
        extern "system" fn(
            FileName: LPCWSTR,
            NewFileName: LPCWSTR,
            ReplaceIfExisting: BOOL,
            DokanFileInfo: PDOKAN_FILE_INFO,
        ) -> NTSTATUS,
    >,
    pub SetEndOfFile: Option<
        extern "system" fn(
            FileName: LPCWSTR,
            ByteOffset: LONGLONG,
            DokanFileInfo: PDOKAN_FILE_INFO,
        ) -> NTSTATUS,
    >,
    pub SetAllocationSize: Option<
        extern "system" fn(
            FileName: LPCWSTR,
            AllocSize: LONGLONG,
            DokanFileInfo: PDOKAN_FILE_INFO,
        ) -> NTSTATUS,
    >,
    pub LockFile: Option<
        extern "system" fn(
            FileName: LPCWSTR,
            ByteOffset: LONGLONG,
            Length: LONGLONG,
            DokanFileInfo: PDOKAN_FILE_INFO,
        ) -> NTSTATUS,
    >,
    pub UnlockFile: Option<
        extern "system" fn(
            FileName: LPCWSTR,
            ByteOffset: LONGLONG,
            Length: LONGLONG,
            DokanFileInfo: PDOKAN_FILE_INFO,
        ) -> NTSTATUS,
    >,
    pub GetDiskFreeSpace: Option<
        extern "system" fn(
            FreeBytesAvailable: PULONGLONG,
            TotalNumberOfBytes: PULONGLONG,
            TotalNumberOfFreeBytes: PULONGLONG,
            DokanFileInfo: PDOKAN_FILE_INFO,
        ) -> NTSTATUS,
    >,
    pub GetVolumeInformation: Option<
        extern "system" fn(
            VolumeNameBuffer: LPWSTR,
            VolumeNameSize: DWORD,
            VolumeSerialNumber: LPDWORD,
            MaximumComponentLength: LPDWORD,
            FileSystemFlags: LPDWORD,
            FileSystemNameBuffer: LPWSTR,
            FileSystemNameSize: DWORD,
            DokanFileInfo: PDOKAN_FILE_INFO,
        ) -> NTSTATUS,
    >,
    pub Mounted: Option<
        extern "system" fn(MountPoint: LPCWSTR, DokanFileInfo: PDOKAN_FILE_INFO) -> NTSTATUS,
    >,
    pub Unmounted: Option<extern "system" fn(DokanFileInfo: PDOKAN_FILE_INFO) -> NTSTATUS>,
    pub GetFileSecurity: Option<
        extern "system" fn(
            FileName: LPCWSTR,
            PSECURITY_INFORMATION: PSECURITY_INFORMATION,
            PSECURITY_DESCRIPTOR: PSECURITY_DESCRIPTOR,
            BufferLength: ULONG,
            LengthNeeded: PULONG,
            DokanFileInfo: PDOKAN_FILE_INFO,
        ) -> NTSTATUS,
    >,
    pub SetFileSecurity: Option<
        extern "system" fn(
            FileName: LPCWSTR,
            SecurityInformation: PSECURITY_INFORMATION,
            SecurityDescriptor: PSECURITY_DESCRIPTOR,
            BufferLength: ULONG,
            DokanFileInfo: PDOKAN_FILE_INFO,
        ) -> NTSTATUS,
    >,
    pub FindStreams: Option<
        extern "system" fn(
            FileName: LPCWSTR,
            FillFindStreamData: PFillFindStreamData,
            FindStreamContext: PVOID,
            DokanFileInfo: PDOKAN_FILE_INFO,
        ) -> NTSTATUS,
    >,
}
