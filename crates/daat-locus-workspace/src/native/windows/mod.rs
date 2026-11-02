//! Uses the installed, upstream-signed Dokan 2 driver. No driver installation,
//! certificate import or privilege escalation happens in this runtime path.
mod ffi;

use super::{
    Mount,
    view::{DELETE, READ, View, WRITE},
};
use crate::{Entry, EntryKind, Error, Result, Workspace};
use ffi::*;
use libloading::os::windows::Library;
use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    io, mem,
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        fs::OpenOptionsExt,
        io::AsRawHandle,
    },
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Path, PathBuf},
    ptr,
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};
use winapi::{
    shared::ntstatus::*,
    shared::{
        minwindef::{BOOL, DWORD, FILETIME, LPDWORD},
        ntdef::{LONGLONG, LPCWSTR, LPWSTR, NTSTATUS},
    },
    um::{
        fileapi::{
            BY_HANDLE_FILE_INFORMATION, GetDiskFreeSpaceExW, GetFileInformationByHandle,
            GetFinalPathNameByHandleW,
        },
        minwinbase::WIN32_FIND_DATAW,
        sysinfoapi::GetSystemDirectoryW,
        winbase::FILE_FLAG_BACKUP_SEMANTICS,
        winnt::*,
    },
};

type Create = unsafe extern "system" fn(
    *mut DOKAN_OPTIONS,
    *mut DOKAN_OPERATIONS,
    *mut *mut std::ffi::c_void,
) -> i32;
type Close = unsafe extern "system" fn(*mut std::ffi::c_void);
type Version = unsafe extern "system" fn() -> u32;

struct Api {
    _library: Library,
    create: Create,
    close: Close,
    library_version: Version,
    driver_version: Version,
}

fn api() -> Result<Option<Arc<Api>>> {
    // Dokan's process initialization is shared by every mount. Keep its DLL
    // resident until process exit so no dispatcher can outlive the function table.
    static API: OnceLock<Mutex<Option<Arc<Api>>>> = OnceLock::new();
    let mut cached = API.get_or_init(|| Mutex::new(None)).lock();
    if let Some(api) = cached.as_ref() {
        return Ok(Some(api.clone()));
    }
    let mut system = vec![0u16; 32_768];
    let length = unsafe { GetSystemDirectoryW(system.as_mut_ptr(), system.len() as u32) } as usize;
    if length == 0 || length >= system.len() {
        return Err(io::Error::last_os_error().into());
    }
    let path = std::path::PathBuf::from(std::ffi::OsString::from_wide(&system[..length]))
        .join("dokan2.dll");
    match std::fs::metadata(&path) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    let library = unsafe { Library::load_with_flags(&path, 0x800) }.map_err(|error| {
        Error::Unsupported(format!("cannot load installed Dokan library: {error}"))
    })?;
    unsafe {
        let init = *library
            .get::<unsafe extern "system" fn()>(b"DokanInit\0")
            .map_err(symbol_error)?;
        let api = Arc::new(Api {
            create: *library
                .get(b"DokanCreateFileSystem\0")
                .map_err(symbol_error)?,
            close: *library.get(b"DokanCloseHandle\0").map_err(symbol_error)?,
            library_version: *library.get(b"DokanVersion\0").map_err(symbol_error)?,
            driver_version: *library.get(b"DokanDriverVersion\0").map_err(symbol_error)?,
            _library: library,
        });
        init();
        *cached = Some(api.clone());
        Ok(Some(api))
    }
}

fn symbol_error(error: libloading::Error) -> Error {
    Error::Unsupported(format!("Dokan 2 API: {error}"))
}

pub(super) fn available() -> Result<bool> {
    let Some(api) = api()? else {
        return Ok(false);
    };
    let version = unsafe { (api.library_version)() };
    // The DLL's public ABI (e.g. 231) and kernel protocol (e.g. 0x190)
    // have different version schemes. The mount handshake verifies that the
    // installed DLL and driver agree; the runtime probe verifies actual I/O.
    Ok((206..300).contains(&version) && unsafe { (api.driver_version)() } != 0)
}

struct Handler {
    view: View,
    security: Vec<u8>,
    serial: u32,
}

pub(super) struct Session {
    api: Arc<Api>,
    handle: usize,
    _point: Vec<u16>,
    _options: Box<DOKAN_OPTIONS>,
    _operations: Box<DOKAN_OPERATIONS>,
    _handler: Box<Handler>,
    registration: Option<(PathBuf, Vec<u16>)>,
}

fn mounted_directories() -> &'static Mutex<HashMap<PathBuf, Vec<u16>>> {
    static MOUNTS: OnceLock<Mutex<HashMap<PathBuf, Vec<u16>>>> = OnceLock::new();
    MOUNTS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn device_path(file: &std::fs::File) -> io::Result<Vec<u16>> {
    const VOLUME_NAME_NT: DWORD = 0x2;
    let mut path = vec![0u16; 32_768];
    let length = unsafe {
        GetFinalPathNameByHandleW(
            file.as_raw_handle().cast(),
            path.as_mut_ptr(),
            path.len() as u32,
            VOLUME_NAME_NT,
        )
    } as usize;
    if length == 0 {
        return Err(io::Error::last_os_error());
    }
    if length >= path.len() {
        return Err(io::Error::other(
            "mounted directory device path is too long",
        ));
    }
    path.truncate(length);
    Ok(path)
}

/// This is an identity check, never a filesystem permission grant. The host
/// broker pins the reparse point against writes/deletion before calling it.
pub(super) fn open_mount_directory(path: &Path) -> io::Result<Option<std::fs::File>> {
    let Some(expected) = mounted_directories()
        .lock()
        .get(dunce::simplified(path))
        .cloned()
    else {
        return Ok(None);
    };
    let file = std::fs::OpenOptions::new()
        .access_mode(FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?;
    if device_path(&file)? != expected {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "workspace mount target was replaced",
        ));
    }
    Ok(Some(file))
}

// Dokan dispatches callbacks concurrently and its lifetime functions are
// thread-safe. The stable boxes remain alive until CloseHandle has joined them.
unsafe impl Send for Session {}
unsafe impl Sync for Session {}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some((path, identity)) = &self.registration {
            let mut mounts = mounted_directories().lock();
            if mounts.get(path) == Some(identity) {
                mounts.remove(path);
            }
        }
        unsafe {
            (self.api.close)(self.handle as *mut _);
        }
    }
}

pub(super) fn mount(workspace: Workspace, path: &Path) -> Result<Mount> {
    let api = api()?.ok_or_else(|| Error::Unsupported("Dokan 2 is not installed".into()))?;
    if !available()? {
        return Err(Error::Unsupported("Dokan 2 driver is unavailable".into()));
    }
    let security = owner_security()?;
    let mut identity = Sha256::new();
    for unit in workspace.backing_path().as_os_str().encode_wide() {
        identity.update(unit.to_le_bytes());
    }
    identity.update(workspace.key().as_bytes());
    let serial = u32::from_le_bytes(identity.finalize()[..4].try_into().expect("SHA256 prefix"));
    let handler = Box::new(Handler {
        view: View::new(workspace)?,
        security,
        serial,
    });
    // The Win32 Dokan API expects DOS/UNC spelling, while service profile paths
    // and stored authorization bindings use std::fs::canonicalize's prefix.
    let point: Vec<_> = dunce::simplified(path)
        .as_os_str()
        .encode_wide()
        .chain([0])
        .collect();
    let mut options: Box<DOKAN_OPTIONS> = Box::new(unsafe { mem::zeroed() });
    options.Version = 206;
    options.Options =
        DOKAN_OPTION_CURRENT_SESSION | DOKAN_OPTION_CASE_SENSITIVE | DOKAN_OPTION_MOUNT_MANAGER;
    options.GlobalContext = (&*handler as *const Handler) as u64;
    options.MountPoint = point.as_ptr();
    options.Timeout = 60_000;
    options.AllocationUnitSize = 4096;
    options.SectorSize = 512;
    if handler.security.len() > options.VolumeSecurityDescriptor.len() {
        return Err(Error::Invalid(
            "owner security descriptor is too large".into(),
        ));
    }
    options.VolumeSecurityDescriptorLength = handler.security.len() as u32;
    unsafe {
        ptr::copy_nonoverlapping(
            handler.security.as_ptr(),
            options.VolumeSecurityDescriptor.as_mut_ptr().cast(),
            handler.security.len(),
        );
    }
    let mut operations: Box<DOKAN_OPERATIONS> = Box::new(unsafe { mem::zeroed() });
    operations.ZwCreateFile = Some(create);
    operations.Cleanup = Some(cleanup);
    operations.CloseFile = Some(close_file);
    operations.ReadFile = Some(read);
    operations.WriteFile = Some(write);
    operations.FlushFileBuffers = Some(flush);
    operations.GetFileInformation = Some(file_information);
    operations.FindFiles = Some(find_files);
    operations.SetFileAttributes = Some(set_attributes);
    operations.SetFileTime = Some(set_time);
    operations.DeleteFile = Some(delete_file);
    operations.DeleteDirectory = Some(delete_directory);
    operations.MoveFile = Some(rename);
    operations.SetEndOfFile = Some(truncate);
    operations.SetAllocationSize = Some(allocate);
    operations.GetDiskFreeSpace = Some(disk_space);
    operations.GetVolumeInformation = Some(volume_information);
    operations.GetFileSecurity = Some(get_security);
    operations.SetFileSecurity = Some(set_security);
    let mut handle = ptr::null_mut();
    let result = unsafe { (api.create)(&mut *options, &mut *operations, &mut handle) };
    if result != 0 {
        return Err(Error::Unsupported(format!("Dokan mount failed ({result})")));
    }
    let mut session = Session {
        api,
        handle: handle as usize,
        _point: point,
        _options: options,
        _operations: operations,
        _handler: handler,
        registration: None,
    };
    // DokanCreateFileSystem may return before Windows has finished attaching
    // the volume behind a directory reparse point. Check the mounted volume's
    // identity, rather than exposing its empty backing directory or relying on
    // a fixed delay. Only read-only readiness observations are retried.
    let directory = wait_ready(path, serial)?;
    let identity = device_path(&directory)?;
    let path = dunce::simplified(path).to_owned();
    mounted_directories()
        .lock()
        .insert(path.clone(), identity.clone());
    session.registration = Some((path, identity));
    Ok(Mount { _session: session })
}

fn wait_ready(path: &Path, serial: u32) -> Result<std::fs::File> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let result = (|| -> io::Result<Option<std::fs::File>> {
            let directory = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
                .open(path)?;
            let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { mem::zeroed() };
            if unsafe { GetFileInformationByHandle(directory.as_raw_handle().cast(), &mut info) }
                == 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok((info.dwVolumeSerialNumber == serial
                && info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0)
                .then_some(directory))
        })();
        match result {
            Ok(Some(directory)) => return Ok(directory),
            Ok(None) => {}
            Err(error) if matches!(error.raw_os_error(), Some(1 | 2 | 3 | 21 | 87)) => {}
            Err(error) => return Err(error.into()),
        }
        if Instant::now() >= deadline {
            return Err(Error::Unsupported(
                "Dokan directory mount did not become ready".into(),
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn status(error: Error) -> NTSTATUS {
    match error {
        Error::Io(error) => match error.kind() {
            io::ErrorKind::NotFound => STATUS_OBJECT_NAME_NOT_FOUND,
            io::ErrorKind::AlreadyExists => STATUS_OBJECT_NAME_COLLISION,
            io::ErrorKind::PermissionDenied => STATUS_ACCESS_DENIED,
            io::ErrorKind::WouldBlock => STATUS_SHARING_VIOLATION,
            io::ErrorKind::NotADirectory => STATUS_NOT_A_DIRECTORY,
            io::ErrorKind::DirectoryNotEmpty => STATUS_DIRECTORY_NOT_EMPTY,
            _ => STATUS_IO_DEVICE_ERROR,
        },
        Error::Invalid(_) => winapi::shared::ntstatus::STATUS_INVALID_PARAMETER,
        Error::Authorization(_) => STATUS_ACCESS_DENIED,
        Error::Conflict(_) => STATUS_FILE_LOCK_CONFLICT,
        Error::Unsupported(_) => STATUS_NOT_SUPPORTED,
        _ => STATUS_FILE_CORRUPT_ERROR,
    }
}

fn call(
    info: PDOKAN_FILE_INFO,
    operation: impl FnOnce(&Handler, &mut DOKAN_FILE_INFO) -> Result<()>,
) -> NTSTATUS {
    match catch_unwind(AssertUnwindSafe(|| unsafe {
        let info = info
            .as_mut()
            .ok_or_else(|| Error::Invalid("missing native request".into()))?;
        let options = info
            .DokanOptions
            .as_ref()
            .ok_or_else(|| Error::Invalid("missing native options".into()))?;
        let handler = (options.GlobalContext as *const Handler)
            .as_ref()
            .ok_or_else(|| Error::Invalid("missing native handler".into()))?;
        operation(handler, info)
    })) {
        Ok(Ok(())) => STATUS_SUCCESS,
        Ok(Err(error)) => status(error),
        Err(_) => STATUS_INTERNAL_ERROR,
    }
}

fn path(pointer: LPCWSTR) -> Result<String> {
    if pointer.is_null() {
        return Err(Error::Invalid("null path".into()));
    }
    let mut length = 0;
    while length < 32_768 && unsafe { *pointer.add(length) } != 0 {
        length += 1;
    }
    if length == 32_768 {
        return Err(Error::Invalid("native path is not terminated".into()));
    }
    let path = String::from_utf16(unsafe { std::slice::from_raw_parts(pointer, length) })
        .map_err(|_| Error::Invalid("native path is not Unicode".into()))?;
    let path = path.trim_start_matches('\\').replace('\\', "/");
    if path.contains(':') {
        return Err(Error::Unsupported("alternate data streams".into()));
    }
    crate::directory::relative_path(&path)?;
    Ok(path)
}

extern "system" fn create(
    name: LPCWSTR,
    _: PDOKAN_IO_SECURITY_CONTEXT,
    access: u32,
    _: u32,
    share: u32,
    disposition: u32,
    options: u32,
    info: PDOKAN_FILE_INFO,
) -> NTSTATUS {
    call(info, |handler, info| {
        let path = path(name)?;
        let existing = match handler.view.lookup(&path) {
            Ok(entry) => Some(entry),
            Err(Error::Io(error)) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        if existing.is_none() && matches!(disposition, 1 | 4) {
            return Err(crate::missing(path));
        }
        if existing.is_some() && disposition == 2 {
            return Err(io::Error::from(io::ErrorKind::AlreadyExists).into());
        }
        if disposition > 5 {
            return Err(Error::Invalid("file create disposition".into()));
        }
        let entry = match existing {
            Some(entry) => entry,
            None => handler.view.create(
                &path,
                options & 1 != 0,
                if options & 1 != 0 { 0o755 } else { 0o644 },
            )?,
        };
        if entry.kind == EntryKind::Symlink {
            return Err(Error::Unsupported("Windows reparse points".into()));
        }
        if options & 1 != 0 && entry.kind != EntryKind::Directory {
            return Err(io::Error::from(io::ErrorKind::NotADirectory).into());
        }
        if options & 0x40 != 0 && entry.kind == EntryKind::Directory {
            return Err(Error::Invalid("file is a directory".into()));
        }
        let mut rights = 0;
        if access & (GENERIC_READ | GENERIC_ALL | FILE_READ_DATA) != 0 {
            rights |= READ;
        }
        if access & (GENERIC_WRITE | GENERIC_ALL | FILE_WRITE_DATA | FILE_APPEND_DATA) != 0 {
            rights |= WRITE;
        }
        if access & (GENERIC_ALL | winapi::um::winnt::DELETE) != 0 {
            rights |= DELETE;
        }
        let mut sharing = 0;
        if share & FILE_SHARE_READ != 0 {
            sharing |= READ;
        }
        if share & FILE_SHARE_WRITE != 0 {
            sharing |= WRITE;
        }
        if share & FILE_SHARE_DELETE != 0 {
            sharing |= DELETE;
        }
        let handle = handler.view.open(
            &path,
            rights,
            sharing,
            access & FILE_APPEND_DATA != 0 && access & FILE_WRITE_DATA == 0,
        )?;
        if entry.kind == EntryKind::File && matches!(disposition, 0 | 4 | 5) && rights & WRITE == 0
        {
            handler.view.close(handle);
            return Err(io::Error::from(io::ErrorKind::PermissionDenied).into());
        }
        if entry.kind == EntryKind::File
            && matches!(disposition, 0 | 4 | 5)
            && let Err(error) = handler.view.truncate(entry.inode, 0)
        {
            handler.view.close(handle);
            return Err(error);
        }
        info.Context = handle;
        info.IsDirectory = u8::from(entry.kind == EntryKind::Directory);
        Ok(())
    })
}

extern "system" fn cleanup(_: LPCWSTR, info: PDOKAN_FILE_INFO) {
    call(info, |handler, info| {
        if info.DeleteOnClose != 0 && info.Context != 0 {
            let path = handler.view.handle_path(info.Context)?;
            handler
                .view
                .remove(&path, info.IsDirectory != 0, Some(info.Context))?;
        }
        Ok(())
    });
}
extern "system" fn close_file(_: LPCWSTR, info: PDOKAN_FILE_INFO) {
    call(info, |handler, info| {
        handler.view.close(info.Context);
        info.Context = 0;
        Ok(())
    });
}
extern "system" fn read(
    _: LPCWSTR,
    buffer: *mut std::ffi::c_void,
    length: u32,
    read: LPDWORD,
    offset: i64,
    info: PDOKAN_FILE_INFO,
) -> NTSTATUS {
    call(info, |handler, info| {
        if offset < 0 || read.is_null() || (length > 0 && buffer.is_null()) {
            return Err(Error::Invalid("invalid read buffer or offset".into()));
        }
        let bytes = handler
            .view
            .read(info.Context, offset as u64, length as usize)?;
        unsafe {
            if !bytes.is_empty() {
                ptr::copy_nonoverlapping(bytes.as_ptr(), buffer.cast(), bytes.len());
            }
            *read = bytes.len() as u32;
        }
        Ok(())
    })
}
extern "system" fn write(
    _: LPCWSTR,
    buffer: *const std::ffi::c_void,
    length: u32,
    written: LPDWORD,
    offset: i64,
    info: PDOKAN_FILE_INFO,
) -> NTSTATUS {
    call(info, |handler, info| {
        if written.is_null()
            || (length > 0 && buffer.is_null())
            || (offset < 0 && info.WriteToEndOfFile == 0)
        {
            return Err(Error::Invalid("invalid write buffer or offset".into()));
        }
        let bytes = if length == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(buffer.cast(), length as usize) }
        };
        let length = if info.PagingIo != 0 {
            handler
                .view
                .write_paging(info.Context, offset.max(0) as u64, bytes)?
        } else {
            handler.view.write(
                info.Context,
                offset.max(0) as u64,
                bytes,
                info.WriteToEndOfFile != 0,
            )?
        };
        unsafe {
            *written = length as u32;
        }
        Ok(())
    })
}
extern "system" fn flush(_: LPCWSTR, info: PDOKAN_FILE_INFO) -> NTSTATUS {
    call(info, |_, _| Ok(()))
}

fn attributes(entry: &Entry) -> DWORD {
    let mut flags = if entry.kind == EntryKind::Directory {
        FILE_ATTRIBUTE_DIRECTORY
    } else {
        0
    };
    if entry.mode & 0o222 == 0 {
        flags |= FILE_ATTRIBUTE_READONLY;
    }
    if flags == 0 {
        FILE_ATTRIBUTE_NORMAL
    } else {
        flags
    }
}
fn file_time(entry: &Entry) -> FILETIME {
    let ticks = (entry.modified_ns.max(0) as u64 / 100) + 116_444_736_000_000_000;
    FILETIME {
        dwLowDateTime: ticks as u32,
        dwHighDateTime: (ticks >> 32) as u32,
    }
}
extern "system" fn file_information(
    name: LPCWSTR,
    buffer: *mut BY_HANDLE_FILE_INFORMATION,
    info: PDOKAN_FILE_INFO,
) -> NTSTATUS {
    call(info, |handler, info| {
        let entry = if info.Context == 0 {
            handler.view.lookup(&path(name)?)?
        } else {
            handler
                .view
                .entry(handler.view.handle_inode(info.Context)?)?
        };
        let buffer = unsafe { buffer.as_mut() }
            .ok_or_else(|| Error::Invalid("missing file information buffer".into()))?;
        *buffer = BY_HANDLE_FILE_INFORMATION {
            dwFileAttributes: attributes(&entry),
            ftCreationTime: file_time(&entry),
            ftLastAccessTime: file_time(&entry),
            ftLastWriteTime: file_time(&entry),
            dwVolumeSerialNumber: handler.serial,
            nFileSizeHigh: (entry.size >> 32) as u32,
            nFileSizeLow: entry.size as u32,
            nNumberOfLinks: 1,
            nFileIndexHigh: (entry.inode >> 32) as u32,
            nFileIndexLow: entry.inode as u32,
        };
        Ok(())
    })
}
extern "system" fn find_files(
    name: LPCWSTR,
    fill: PFillFindData,
    info: PDOKAN_FILE_INFO,
) -> NTSTATUS {
    call(info, |handler, _| {
        let entry = handler.view.lookup(&path(name)?)?;
        for (name, entry) in handler.view.read_dir(entry.inode)? {
            let name: Vec<_> = name.encode_utf16().collect();
            if name.len() >= 260 {
                return Err(Error::Invalid("directory entry name is too long".into()));
            }
            let mut data: WIN32_FIND_DATAW = unsafe { mem::zeroed() };
            data.dwFileAttributes = attributes(&entry);
            data.ftCreationTime = file_time(&entry);
            data.ftLastAccessTime = file_time(&entry);
            data.ftLastWriteTime = file_time(&entry);
            data.nFileSizeHigh = (entry.size >> 32) as u32;
            data.nFileSizeLow = entry.size as u32;
            data.cFileName[..name.len()].copy_from_slice(&name);
            if unsafe { fill(&mut data, info) } != 0 {
                break;
            }
        }
        Ok(())
    })
}
extern "system" fn set_attributes(name: LPCWSTR, flags: u32, info: PDOKAN_FILE_INFO) -> NTSTATUS {
    call(info, |handler, _| {
        let entry = handler.view.lookup(&path(name)?)?;
        handler.view.set_metadata(
            entry.inode,
            Some(if flags & FILE_ATTRIBUTE_READONLY != 0 {
                entry.mode & !0o222
            } else {
                entry.mode | 0o200
            }),
            None,
        )
    })
}
extern "system" fn set_time(
    name: LPCWSTR,
    _: *const FILETIME,
    _: *const FILETIME,
    modified: *const FILETIME,
    info: PDOKAN_FILE_INFO,
) -> NTSTATUS {
    call(info, |handler, _| {
        if let Some(time) = unsafe { modified.as_ref() } {
            let ticks = ((time.dwHighDateTime as u64) << 32) | time.dwLowDateTime as u64;
            if ticks != 0 && ticks != u64::MAX {
                let ns = ticks
                    .saturating_sub(116_444_736_000_000_000)
                    .saturating_mul(100)
                    .min(i64::MAX as u64) as i64;
                handler.view.set_metadata(
                    handler.view.lookup(&path(name)?)?.inode,
                    None,
                    Some(ns),
                )?;
            }
        }
        Ok(())
    })
}
extern "system" fn delete_file(name: LPCWSTR, info: PDOKAN_FILE_INFO) -> NTSTATUS {
    call(info, |handler, info| {
        handler
            .view
            .can_remove(
                &path(name)?,
                false,
                (info.Context != 0).then_some(info.Context),
            )
            .map(|_| ())
    })
}
extern "system" fn delete_directory(name: LPCWSTR, info: PDOKAN_FILE_INFO) -> NTSTATUS {
    call(info, |handler, info| {
        handler
            .view
            .can_remove(
                &path(name)?,
                true,
                (info.Context != 0).then_some(info.Context),
            )
            .map(|_| ())
    })
}
extern "system" fn rename(
    name: LPCWSTR,
    target: LPCWSTR,
    replace: BOOL,
    info: PDOKAN_FILE_INFO,
) -> NTSTATUS {
    call(info, |handler, info| {
        handler.view.rename(
            &path(name)?,
            &path(target)?,
            replace != 0,
            (info.Context != 0).then_some(info.Context),
        )
    })
}
extern "system" fn truncate(name: LPCWSTR, length: LONGLONG, info: PDOKAN_FILE_INFO) -> NTSTATUS {
    call(info, |handler, info| {
        if length < 0 {
            return Err(Error::Invalid("negative file size".into()));
        }
        let inode = if info.Context == 0 {
            handler.view.lookup(&path(name)?)?.inode
        } else {
            handler.view.handle_inode(info.Context)?
        };
        handler.view.truncate(inode, length as u64)
    })
}
extern "system" fn allocate(name: LPCWSTR, length: LONGLONG, info: PDOKAN_FILE_INFO) -> NTSTATUS {
    call(info, |handler, _| {
        let entry = handler.view.lookup(&path(name)?)?;
        if length < 0 {
            return Err(Error::Invalid("negative allocation size".into()));
        }
        if (length as u64) < entry.size {
            handler.view.truncate(entry.inode, length as u64)?;
        }
        Ok(())
    })
}
extern "system" fn disk_space(
    available: *mut u64,
    total: *mut u64,
    free: *mut u64,
    info: PDOKAN_FILE_INFO,
) -> NTSTATUS {
    call(info, |handler, _| {
        let path = handler
            .view
            .backing_path()
            .parent()
            .ok_or_else(|| Error::Invalid("missing storage directory".into()))?;
        let path: Vec<_> = path.as_os_str().encode_wide().chain([0]).collect();
        if unsafe {
            GetDiskFreeSpaceExW(path.as_ptr(), available.cast(), total.cast(), free.cast())
        } == 0
        {
            return Err(io::Error::last_os_error().into());
        }
        Ok(())
    })
}
extern "system" fn volume_information(
    label: LPWSTR,
    label_length: DWORD,
    serial: LPDWORD,
    max_component: LPDWORD,
    flags: LPDWORD,
    fs_name: LPWSTR,
    fs_length: DWORD,
    info: PDOKAN_FILE_INFO,
) -> NTSTATUS {
    call(info, |handler, _| {
        fn copy(text: &str, target: LPWSTR, length: u32) -> Result<()> {
            let text: Vec<_> = text.encode_utf16().chain([0]).collect();
            if target.is_null() || text.len() > length as usize {
                return Err(Error::Invalid(
                    "volume information buffer is too small".into(),
                ));
            }
            unsafe {
                ptr::copy_nonoverlapping(text.as_ptr(), target, text.len());
            }
            Ok(())
        }
        copy("DL Workspace", label, label_length)?;
        copy("DaatLocus", fs_name, fs_length)?;
        unsafe {
            if !serial.is_null() {
                *serial = handler.serial;
            }
            if !max_component.is_null() {
                *max_component = 255;
            }
            if !flags.is_null() {
                *flags =
                    FILE_CASE_SENSITIVE_SEARCH | FILE_CASE_PRESERVED_NAMES | FILE_UNICODE_ON_DISK;
            }
        }
        Ok(())
    })
}
extern "system" fn get_security(
    _: LPCWSTR,
    _: *mut u32,
    buffer: *mut std::ffi::c_void,
    length: u32,
    needed: *mut u32,
    info: PDOKAN_FILE_INFO,
) -> NTSTATUS {
    let mut short = false;
    let result = call(info, |handler, _| {
        if needed.is_null() {
            return Err(Error::Invalid("missing security buffer length".into()));
        }
        unsafe {
            *needed = handler.security.len() as u32;
        }
        if buffer.is_null() || (length as usize) < handler.security.len() {
            short = true;
            return Ok(());
        }
        unsafe {
            ptr::copy_nonoverlapping(
                handler.security.as_ptr(),
                buffer.cast(),
                handler.security.len(),
            );
        }
        Ok(())
    });
    if short {
        STATUS_BUFFER_OVERFLOW
    } else {
        result
    }
}
extern "system" fn set_security(
    _: LPCWSTR,
    _: *mut u32,
    _: *mut std::ffi::c_void,
    _: u32,
    _: PDOKAN_FILE_INFO,
) -> NTSTATUS {
    STATUS_ACCESS_DENIED
}

fn owner_security() -> Result<Vec<u8>> {
    use winapi::shared::sddl::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
    };
    use winapi::um::{
        handleapi::CloseHandle,
        processthreadsapi::{GetCurrentProcess, OpenProcessToken},
        securitybaseapi::{GetSecurityDescriptorLength, GetTokenInformation},
        winbase::LocalFree,
    };
    unsafe {
        let mut token = ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(io::Error::last_os_error().into());
        }
        struct Token(winapi::shared::ntdef::HANDLE);
        impl Drop for Token {
            fn drop(&mut self) {
                unsafe {
                    CloseHandle(self.0);
                }
            }
        }
        let token = Token(token);
        let mut size = 0;
        GetTokenInformation(token.0, TokenUser, ptr::null_mut(), 0, &mut size);
        let mut bytes = vec![0usize; (size as usize).div_ceil(mem::size_of::<usize>())];
        if GetTokenInformation(
            token.0,
            TokenUser,
            bytes.as_mut_ptr().cast(),
            size,
            &mut size,
        ) == 0
        {
            return Err(io::Error::last_os_error().into());
        }
        let user = &*(bytes.as_ptr() as *const TOKEN_USER);
        let mut sid = ptr::null_mut();
        if ConvertSidToStringSidW(user.User.Sid, &mut sid) == 0 {
            return Err(io::Error::last_os_error().into());
        }
        let mut length = 0;
        while *sid.add(length) != 0 {
            length += 1;
        }
        let sid_text = String::from_utf16_lossy(std::slice::from_raw_parts(sid, length));
        LocalFree(sid.cast());
        let sddl: Vec<_> = format!("O:{sid_text}G:{sid_text}D:P(A;;FA;;;SY)(A;;FA;;;{sid_text})")
            .encode_utf16()
            .chain([0])
            .collect();
        let mut descriptor = ptr::null_mut();
        let mut size = 0;
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1,
            &mut descriptor,
            &mut size,
        ) == 0
        {
            return Err(io::Error::last_os_error().into());
        }
        let length = GetSecurityDescriptorLength(descriptor) as usize;
        let bytes = std::slice::from_raw_parts(descriptor.cast::<u8>(), length).to_vec();
        LocalFree(descriptor.cast());
        Ok(bytes)
    }
}
