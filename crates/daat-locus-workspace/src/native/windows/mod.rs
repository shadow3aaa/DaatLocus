//! WinFsp's upstream-signed driver supplies Windows filesystem semantics.
mod ffi;
use super::{
    Mount,
    view::{DELETE, READ, SHARE_ALL, View, WRITE},
};
use crate::{Entry, EntryKind, Error, Result, Workspace, reparse::ReparsePoint};
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
    shared::{minwindef::DWORD, ntstatus::*},
    um::{
        fileapi::{
            BY_HANDLE_FILE_INFORMATION, GetDiskFreeSpaceExW, GetFileInformationByHandle,
            GetFinalPathNameByHandleW,
        },
        winbase::FILE_FLAG_BACKUP_SEMANTICS,
        winnt::*,
        winreg::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RRF_SUBKEY_WOW6432KEY, RegGetValueW},
    },
};
struct Api {
    _library: Library,
    preflight: unsafe extern "C" fn(Name, Name) -> Status,
    create: unsafe extern "C" fn(Name, *const VolumeParams, *const Interface, *mut Fs) -> Status,
    start: unsafe extern "C" fn(Fs, u32) -> Status,
    stop: unsafe extern "C" fn(Fs),
    delete: unsafe extern "C" fn(Fs),
    add_directory: unsafe extern "C" fn(*const DirInfo, Buffer, u32, *mut u32) -> u8,
    find_reparse: unsafe extern "C" fn(Fs, GetReparseByName, Context, Name, *mut u32) -> u8,
    resolve_reparse: unsafe extern "C" fn(
        Fs,
        GetReparseByName,
        Context,
        Name,
        u32,
        u8,
        Buffer,
        Buffer,
        *mut usize,
    ) -> Status,
}
fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain([0]).collect()
}
fn api() -> Result<Option<Arc<Api>>> {
    static API: OnceLock<Mutex<Option<Arc<Api>>>> = OnceLock::new();
    let mut cached = API.get_or_init(|| Mutex::new(None)).lock();
    if let Some(api) = &*cached {
        return Ok(Some(api.clone()));
    }
    let key = wide(r"SOFTWARE\WinFsp");
    let value = wide("InstallDir");
    let mut path = vec![0u16; 32_768];
    let mut length = (path.len() * 2) as u32;
    let result = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ | RRF_SUBKEY_WOW6432KEY,
            ptr::null_mut(),
            path.as_mut_ptr().cast(),
            &mut length,
        )
    };
    if result == 2 || result == 3 {
        return Ok(None);
    }
    if result != 0 {
        return Err(io::Error::from_raw_os_error(result).into());
    }
    let count = (length as usize / 2).min(path.len());
    let count = path[..count]
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(count);
    let root = PathBuf::from(std::ffi::OsString::from_wide(&path[..count]));
    if !root.is_absolute() {
        return Err(Error::Unsupported(
            "WinFsp installation path is not absolute".into(),
        ));
    }
    #[cfg(target_arch = "x86_64")]
    let name = "winfsp-x64.dll";
    #[cfg(target_arch = "aarch64")]
    let name = "winfsp-a64.dll";
    #[cfg(target_arch = "x86")]
    let name = "winfsp-x86.dll";
    let library = unsafe { Library::load_with_flags(root.join("bin").join(name), 0x100 | 0x800) }
        .map_err(|error| {
        Error::Unsupported(format!("cannot load installed WinFsp library: {error}"))
    })?;
    let api = unsafe {
        Arc::new(Api {
            preflight: *library
                .get(b"FspFileSystemPreflight\0")
                .map_err(symbol_error)?,
            create: *library
                .get(b"FspFileSystemCreate\0")
                .map_err(symbol_error)?,
            start: *library
                .get(b"FspFileSystemStartDispatcher\0")
                .map_err(symbol_error)?,
            stop: *library
                .get(b"FspFileSystemStopDispatcher\0")
                .map_err(symbol_error)?,
            delete: *library
                .get(b"FspFileSystemDelete\0")
                .map_err(symbol_error)?,
            add_directory: *library
                .get(b"FspFileSystemAddDirInfo\0")
                .map_err(symbol_error)?,
            find_reparse: *library
                .get(b"FspFileSystemFindReparsePoint\0")
                .map_err(symbol_error)?,
            resolve_reparse: *library
                .get(b"FspFileSystemResolveReparsePoints\0")
                .map_err(symbol_error)?,
            _library: library,
        })
    };
    *cached = Some(api.clone());
    Ok(Some(api))
}
fn symbol_error(error: libloading::Error) -> Error {
    Error::Unsupported(format!("WinFsp API: {error}"))
}
pub(super) fn available() -> Result<bool> {
    let Some(api) = api()? else {
        return Ok(false);
    };
    Ok(unsafe { (api.preflight)(wide("WinFsp.Net").as_mut_ptr(), ptr::null_mut()) } >= 0)
}
struct Handler {
    api: Arc<Api>,
    view: View,
    security: Vec<u8>,
    origin: PathBuf,
    directory: PathBuf,
}
pub(super) struct Session {
    api: Arc<Api>,
    fs: usize,
    _operations: Box<Interface>,
    _handler: Box<Handler>,
    registration: Option<(PathBuf, Vec<u16>)>,
}
// Dispatcher callbacks finish before their stable handler and function table drop.
unsafe impl Send for Session {}
unsafe impl Sync for Session {}
impl Session {
    pub(super) fn retire(&mut self) -> Result<()> {
        self._handler.view.retire()
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        if let Some((path, identity)) = &self.registration {
            let mut mounts = mounted_directories().lock();
            if mounts
                .get(path)
                .is_some_and(|target| &target.path == identity)
            {
                mounts.remove(path);
            }
        }
        unsafe {
            (self.api.stop)(self.fs as Fs);
            (self.api.delete)(self.fs as Fs);
        }
    }
}
fn server(root: &Path) -> String {
    let mut identity = Sha256::new();
    for unit in root.as_os_str().encode_wide() {
        identity.update(unit.to_le_bytes());
    }
    format!("DaatLocus-{}", hex::encode(&identity.finalize()[..20]))
}

pub(super) fn directory_path(path: &Path) -> PathBuf {
    PathBuf::from(format!(
        r"\\{}\{}",
        server(path.parent().unwrap_or(path)),
        path.file_name().unwrap_or_default().to_string_lossy()
    ))
}

pub(super) fn logical_path(path: &Path, root: &Path) -> PathBuf {
    use std::path::{Component, Prefix};
    let mut parts = path.components();
    let Some(Component::Prefix(prefix)) = parts.next() else {
        return path.to_owned();
    };
    let (host, share) = match prefix.kind() {
        Prefix::UNC(host, share) | Prefix::VerbatimUNC(host, share) => (host, share),
        _ => return path.to_owned(),
    };
    let share = share.to_string_lossy();
    if !host.to_string_lossy().eq_ignore_ascii_case(&server(root))
        || share.len() != 64
        || !share.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return path.to_owned();
    }
    let mut result = root.join(share.to_ascii_lowercase());
    for part in parts {
        match part {
            Component::RootDir => {}
            Component::Normal(name) => result.push(name),
            _ => return path.to_owned(),
        }
    }
    result
}

pub(super) fn is_mount_path(path: &Path) -> bool {
    unc_root(path).is_some_and(|root| mounted_directories().lock().contains_key(&root))
}

fn unc_root(path: &Path) -> Option<PathBuf> {
    use std::path::{Component, Prefix};
    let Component::Prefix(prefix) = path.components().next()? else {
        return None;
    };
    let (server, share) = match prefix.kind() {
        Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => (server, share),
        _ => return None,
    };
    Some(PathBuf::from(
        format!(
            r"\\{}\{}",
            server.to_string_lossy(),
            share.to_string_lossy()
        )
        .to_ascii_lowercase(),
    ))
}

pub(super) fn mounted_roots() -> Vec<PathBuf> {
    mounted_directories().lock().keys().cloned().collect()
}

pub(super) fn mount(workspace: Workspace, path: &Path) -> Result<Mount> {
    let api = api()?.ok_or_else(|| Error::Unsupported("WinFsp is not installed".into()))?;
    let directory = directory_path(path);
    let path = directory.as_path();
    let mut identity = Sha256::new();
    for unit in workspace.backing_path().as_os_str().encode_wide() {
        identity.update(unit.to_le_bytes());
    }
    identity.update(workspace.key().as_bytes());
    let serial = u32::from_le_bytes(identity.finalize()[..4].try_into().expect("SHA256 prefix"));
    let origin = workspace.origin().to_owned();
    let handler = Box::new(Handler {
        api: api.clone(),
        view: View::new(workspace)?,
        security: owner_security()?,
        origin,
        directory: path.to_owned(),
    });
    let mut params: VolumeParams = unsafe { mem::zeroed() };
    params.sector_size = 512;
    params.sectors_per_allocation_unit = 8;
    params.max_component_length = 255;
    params.serial = serial;
    // Case-sensitive names, owner ACLs, reparse points, per-open contexts.
    // The kernel enforces sharing, byte locks and reparse write access.
    let prefix = format!("\\{}", path.to_string_lossy().trim_start_matches('\\'));
    for (output, unit) in params.prefix.iter_mut().zip(prefix.encode_utf16()) {
        *output = unit;
    }
    params.flags = 0x1 | 0x2 | 0x4 | 0x8 | 0x10 | (1 << 16);
    for (output, unit) in params.name.iter_mut().zip("DaatWorkspace".encode_utf16()) {
        *output = unit;
    }
    let mut operations: Box<Interface> = Box::new(unsafe { mem::zeroed() });
    operations.get_volume_info = Some(get_volume_info);
    operations.get_security_by_name = Some(get_security_by_name);
    operations.create = Some(create);
    operations.open = Some(open);
    operations.overwrite = Some(overwrite);
    operations.cleanup = Some(cleanup);
    operations.close = Some(close);
    operations.read = Some(read);
    operations.write = Some(write);
    operations.flush = Some(flush);
    operations.get_file_info = Some(get_file_info);
    operations.set_basic_info = Some(set_basic_info);
    operations.set_file_size = Some(set_file_size);
    operations.can_delete = Some(can_delete);
    operations.rename = Some(rename);
    operations.get_security = Some(get_security);
    operations.set_security = Some(set_security);
    operations.read_directory = Some(read_directory);
    operations.resolve_reparse_points = Some(resolve_reparse_points);
    operations.get_reparse_point = Some(get_reparse_point);
    operations.set_reparse_point = Some(set_reparse_point);
    operations.delete_reparse_point = Some(delete_reparse_point);
    let mut fs = ptr::null_mut();
    check(
        unsafe {
            (api.create)(
                wide("WinFsp.Net").as_mut_ptr(),
                &params,
                &*operations,
                &mut fs,
            )
        },
        "create filesystem",
    )?;
    unsafe {
        (*fs).user_context = (&*handler as *const Handler).cast_mut().cast();
    }
    let mut session = Session {
        api,
        fs: fs as usize,
        _operations: operations,
        _handler: handler,
        registration: None,
    };
    check(unsafe { (session.api.start)(fs, 0) }, "start dispatcher")?;
    let directory = wait_ready(path, serial)?;
    let identity = device_path(&directory)?;
    let path =
        unc_root(path).ok_or_else(|| Error::Invalid("invalid native projection path".into()))?;
    let kernel = unsafe { &(*fs).volume_name };
    let length = kernel
        .iter()
        .position(|unit| *unit == 0)
        .ok_or_else(|| Error::Corrupt("unterminated WinFsp volume name".into()))?;
    let mut kernel = kernel[..length].to_vec();
    if kernel.last() != Some(&(b'\\' as u16)) {
        kernel.push(b'\\' as u16);
    }
    mounted_directories().lock().insert(
        path.clone(),
        MountedVolume {
            identity,
            path: kernel.clone(),
        },
    );
    session.registration = Some((path, kernel));
    Ok(Mount {
        directory: session._handler.directory.clone(),
        _session: session,
    })
}
fn check(status: Status, operation: &str) -> Result<()> {
    if status >= 0 {
        Ok(())
    } else {
        Err(Error::Unsupported(format!(
            "WinFsp {operation} failed ({status:#010x})"
        )))
    }
}
fn status(error: Error) -> Status {
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
fn call(fs: Fs, operation: impl FnOnce(&Handler) -> Result<Status>) -> Status {
    match catch_unwind(AssertUnwindSafe(|| {
        let handler = unsafe {
            fs.as_ref()
                .and_then(|fs| fs.user_context.cast::<Handler>().as_ref())
        }
        .ok_or_else(|| Error::Invalid("missing native handler".into()))?;
        operation(handler)
    })) {
        Ok(Ok(status)) => status,
        Ok(Err(error)) => status(error),
        Err(_) => STATUS_INTERNAL_ERROR,
    }
}
fn text(pointer: Name) -> Result<String> {
    if pointer.is_null() {
        return Err(Error::Invalid("null native string".into()));
    }
    let mut length = 0;
    while length < 32_768 && unsafe { *pointer.add(length) } != 0 {
        length += 1;
    }
    if length == 32_768 {
        return Err(Error::Invalid("unterminated native string".into()));
    }
    String::from_utf16(unsafe { std::slice::from_raw_parts(pointer, length) })
        .map_err(|_| Error::Invalid("native path is not Unicode".into()))
}
fn path(pointer: Name) -> Result<String> {
    let path = text(pointer)?.trim_start_matches('\\').replace('\\', "/");
    if path.contains(':') {
        return Err(Error::Unsupported("alternate data streams".into()));
    }
    crate::directory::relative_path(&path)?;
    Ok(path)
}
fn handle(context: Context) -> u64 {
    context as usize as u64
}
fn attributes(handler: &Handler, entry: &Entry) -> Result<(u32, u32)> {
    let mut attributes = if entry.mode & 0o222 == 0 {
        FILE_ATTRIBUTE_READONLY
    } else {
        0
    };
    let mut tag = 0;
    if entry.kind == EntryKind::Directory {
        attributes |= FILE_ATTRIBUTE_DIRECTORY;
    }
    if let Some(point) = handler.view.reparse_point(entry.inode)? {
        attributes |= FILE_ATTRIBUTE_REPARSE_POINT;
        if point.directory {
            attributes |= FILE_ATTRIBUTE_DIRECTORY;
        }
        tag = point.tag()?;
    }
    Ok((
        if attributes == 0 {
            FILE_ATTRIBUTE_NORMAL
        } else {
            attributes
        },
        tag,
    ))
}
fn file_info(handler: &Handler, inode: u64, output: *mut FileInfo) -> Result<()> {
    if output.is_null() {
        return Err(Error::Invalid("missing file information".into()));
    }
    let entry = handler.view.entry(inode)?;
    let (attributes, tag) = if entry.kind == EntryKind::Symlink {
        attributes(handler, &entry)?
    } else {
        (
            if entry.kind == EntryKind::Directory {
                FILE_ATTRIBUTE_DIRECTORY
            } else if entry.mode & 0o222 == 0 {
                FILE_ATTRIBUTE_READONLY
            } else {
                FILE_ATTRIBUTE_NORMAL
            },
            0,
        )
    };
    let time = (116_444_736_000_000_000i128 + i128::from(entry.modified_ns) / 100).max(0) as u64;
    let size = if entry.kind == EntryKind::File {
        entry.size
    } else {
        0
    };
    unsafe {
        *output = FileInfo {
            attributes,
            reparse_tag: tag,
            size,
            allocation_size: size.saturating_add(4095) / 4096 * 4096,
            creation_time: time,
            access_time: time,
            write_time: time,
            change_time: time,
            inode,
            ..FileInfo::default()
        };
    }
    Ok(())
}
fn copy_buffer(bytes: &[u8], output: Buffer, length: *mut usize, short: Status) -> Result<Status> {
    let size =
        unsafe { length.as_mut() }.ok_or_else(|| Error::Invalid("missing buffer size".into()))?;
    let available = *size;
    *size = bytes.len();
    if available < bytes.len() {
        return Ok(short);
    }
    if !output.is_null() {
        unsafe {
            ptr::copy_nonoverlapping(bytes.as_ptr(), output.cast(), bytes.len());
        }
    }
    Ok(STATUS_SUCCESS)
}
extern "C" fn get_volume_info(fs: Fs, output: *mut VolumeInfo) -> Status {
    call(fs, |handler| {
        let output = unsafe { output.as_mut() }
            .ok_or_else(|| Error::Invalid("missing volume information".into()))?;
        let mut available: ULARGE_INTEGER = unsafe { mem::zeroed() };
        let mut total: ULARGE_INTEGER = unsafe { mem::zeroed() };
        let mut free: ULARGE_INTEGER = unsafe { mem::zeroed() };
        let source: Vec<_> = handler
            .view
            .backing_path()
            .parent()
            .unwrap_or(Path::new("."))
            .as_os_str()
            .encode_wide()
            .chain([0])
            .collect();
        if unsafe { GetDiskFreeSpaceExW(source.as_ptr(), &mut available, &mut total, &mut free) }
            == 0
        {
            return Err(io::Error::last_os_error().into());
        }
        output.total_size = unsafe { *total.QuadPart() };
        output.free_size = unsafe { *available.QuadPart() };
        let label: Vec<_> = "Workspace".encode_utf16().collect();
        output.label_length = (label.len() * 2) as u16;
        output.label[..label.len()].copy_from_slice(&label);
        Ok(STATUS_SUCCESS)
    })
}
extern "C" fn get_security_by_name(
    fs: Fs,
    name: Name,
    attrs: *mut u32,
    output: Buffer,
    length: *mut usize,
) -> Status {
    call(fs, |handler| {
        let path = path(name)?;
        let entry = match handler.view.lookup(&path) {
            Ok(entry) => entry,
            Err(error) => {
                if unsafe {
                    (handler.api.find_reparse)(
                        fs,
                        get_reparse_by_name,
                        ptr::null_mut(),
                        name,
                        attrs,
                    )
                } != 0
                {
                    return Ok(STATUS_REPARSE);
                }
                return Err(error);
            }
        };
        if let Some(attrs) = unsafe { attrs.as_mut() } {
            *attrs = attributes(handler, &entry)?.0;
        }
        if length.is_null() {
            Ok(STATUS_SUCCESS)
        } else {
            copy_buffer(&handler.security, output, length, STATUS_BUFFER_OVERFLOW)
        }
    })
}
fn open_file(
    handler: &Handler,
    path: &str,
    access: u32,
    context: *mut Context,
    info: *mut FileInfo,
) -> Result<Status> {
    if context.is_null() {
        return Err(Error::Invalid("missing file context".into()));
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
    let id = handler.view.open(
        path,
        rights,
        SHARE_ALL,
        access & FILE_APPEND_DATA != 0 && access & FILE_WRITE_DATA == 0,
    )?;
    if let Err(error) = file_info(handler, handler.view.handle_inode(id)?, info) {
        handler.view.close(id);
        return Err(error);
    }
    unsafe {
        *context = id as usize as Context;
    }
    Ok(STATUS_SUCCESS)
}
extern "C" fn create(
    fs: Fs,
    name: Name,
    options: u32,
    access: u32,
    attrs: u32,
    _: Buffer,
    _: u64,
    context: *mut Context,
    info: *mut FileInfo,
) -> Status {
    call(fs, |handler| {
        let path = path(name)?;
        let directory = options & 1 != 0;
        handler.view.create(
            &path,
            directory,
            if attrs & FILE_ATTRIBUTE_READONLY != 0 {
                0o444
            } else if directory {
                0o755
            } else {
                0o644
            },
        )?;
        open_file(handler, &path, access, context, info)
    })
}
extern "C" fn open(
    fs: Fs,
    name: Name,
    _: u32,
    access: u32,
    context: *mut Context,
    info: *mut FileInfo,
) -> Status {
    call(fs, |handler| {
        open_file(handler, &path(name)?, access, context, info)
    })
}
extern "C" fn overwrite(
    fs: Fs,
    context: Context,
    attrs: u32,
    replace: u8,
    _: u64,
    info: *mut FileInfo,
) -> Status {
    call(fs, |handler| {
        let inode = handler.view.handle_inode(handle(context))?;
        handler.view.truncate(inode, 0)?;
        if replace != 0 {
            handler.view.set_metadata(
                inode,
                Some(if attrs & FILE_ATTRIBUTE_READONLY != 0 {
                    0o444
                } else {
                    0o644
                }),
                None,
            )?;
        }
        file_info(handler, inode, info)?;
        Ok(STATUS_SUCCESS)
    })
}
extern "C" fn cleanup(fs: Fs, context: Context, _: Name, flags: u32) {
    call(fs, |handler| {
        if flags & 1 != 0 {
            let id = handle(context);
            let path = handler.view.handle_path(id)?;
            let entry = handler.view.entry(handler.view.handle_inode(id)?)?;
            // Delete disposition belongs to the kernel file object. Its final
            // cleanup may use an open context without the original DELETE bit.
            handler
                .view
                .remove(&path, entry.kind == EntryKind::Directory, None)?;
        }
        Ok(STATUS_SUCCESS)
    });
}
extern "C" fn close(fs: Fs, context: Context) {
    call(fs, |handler| {
        handler.view.close(handle(context));
        Ok(STATUS_SUCCESS)
    });
}
extern "C" fn read(
    fs: Fs,
    context: Context,
    buffer: Buffer,
    offset: u64,
    length: u32,
    transferred: *mut u32,
) -> Status {
    call(fs, |handler| {
        if transferred.is_null() || (length > 0 && buffer.is_null()) {
            return Err(Error::Invalid("missing read buffer".into()));
        }
        let bytes = handler
            .view
            .read(handle(context), offset, length as usize)?;
        unsafe {
            if !bytes.is_empty() {
                ptr::copy_nonoverlapping(bytes.as_ptr(), buffer.cast(), bytes.len());
            }
            *transferred = bytes.len() as u32;
        }
        Ok(if bytes.is_empty() && length > 0 {
            STATUS_END_OF_FILE
        } else {
            STATUS_SUCCESS
        })
    })
}
extern "C" fn write(
    fs: Fs,
    context: Context,
    buffer: Buffer,
    offset: u64,
    length: u32,
    append: u8,
    constrained: u8,
    transferred: *mut u32,
    info: *mut FileInfo,
) -> Status {
    call(fs, |handler| {
        if transferred.is_null() || (length > 0 && buffer.is_null()) {
            return Err(Error::Invalid("missing write buffer".into()));
        }
        let bytes = if length == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(buffer.cast::<u8>(), length as usize) }
        };
        let id = handle(context);
        let count = if constrained != 0 {
            handler.view.write_paging(id, offset, bytes)?
        } else {
            handler.view.write(id, offset, bytes, append != 0)?
        };
        unsafe {
            *transferred = count as u32;
        }
        file_info(handler, handler.view.handle_inode(id)?, info)?;
        Ok(STATUS_SUCCESS)
    })
}
extern "C" fn flush(fs: Fs, context: Context, info: *mut FileInfo) -> Status {
    call(fs, |handler| {
        if !context.is_null() {
            file_info(handler, handler.view.handle_inode(handle(context))?, info)?;
        }
        Ok(STATUS_SUCCESS)
    })
}
extern "C" fn get_file_info(fs: Fs, context: Context, info: *mut FileInfo) -> Status {
    call(fs, |handler| {
        file_info(handler, handler.view.handle_inode(handle(context))?, info)?;
        Ok(STATUS_SUCCESS)
    })
}
extern "C" fn set_basic_info(
    fs: Fs,
    context: Context,
    attrs: u32,
    _: u64,
    _: u64,
    written: u64,
    _: u64,
    info: *mut FileInfo,
) -> Status {
    call(fs, |handler| {
        let inode = handler.view.handle_inode(handle(context))?;
        let entry = handler.view.entry(inode)?;
        let mode = (attrs != u32::MAX).then_some({
            if attrs & FILE_ATTRIBUTE_READONLY != 0 {
                entry.mode & !0o222
            } else {
                entry.mode | 0o200
            }
        });
        let time = (written != 0).then(|| {
            ((i128::from(written) - 116_444_736_000_000_000) * 100)
                .clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
        });
        handler.view.set_metadata(inode, mode, time)?;
        file_info(handler, inode, info)?;
        Ok(STATUS_SUCCESS)
    })
}
extern "C" fn set_file_size(
    fs: Fs,
    context: Context,
    size: u64,
    allocation: u8,
    info: *mut FileInfo,
) -> Status {
    call(fs, |handler| {
        let inode = handler.view.handle_inode(handle(context))?;
        if allocation == 0 || size < handler.view.entry(inode)?.size {
            handler.view.truncate(inode, size)?;
        }
        file_info(handler, inode, info)?;
        Ok(STATUS_SUCCESS)
    })
}
extern "C" fn can_delete(fs: Fs, context: Context, _: Name) -> Status {
    call(fs, |handler| {
        let id = handle(context);
        let path = handler.view.handle_path(id)?;
        let entry = handler.view.entry(handler.view.handle_inode(id)?)?;
        handler
            .view
            .can_remove(&path, entry.kind == EntryKind::Directory, None)?;
        Ok(STATUS_SUCCESS)
    })
}
extern "C" fn rename(fs: Fs, context: Context, _: Name, target: Name, replace: u8) -> Status {
    call(fs, |handler| {
        let id = handle(context);
        handler.view.rename(
            &handler.view.handle_path(id)?,
            &path(target)?,
            replace != 0,
            None,
        )?;
        Ok(STATUS_SUCCESS)
    })
}
extern "C" fn get_security(fs: Fs, _: Context, buffer: Buffer, size: *mut usize) -> Status {
    call(fs, |handler| {
        copy_buffer(&handler.security, buffer, size, STATUS_BUFFER_OVERFLOW)
    })
}
extern "C" fn set_security(_: Fs, _: Context, _: u32, _: Buffer) -> Status {
    STATUS_ACCESS_DENIED
}
extern "C" fn read_directory(
    fs: Fs,
    context: Context,
    _: Name,
    marker: Name,
    buffer: Buffer,
    length: u32,
    transferred: *mut u32,
) -> Status {
    call(fs, |handler| {
        if transferred.is_null() || (length > 0 && buffer.is_null()) {
            return Err(Error::Invalid("missing directory buffer".into()));
        }
        unsafe {
            *transferred = 0;
        }
        let inode = handler.view.handle_inode(handle(context))?;
        let mut entries = handler.view.read_dir(inode)?;
        let path = handler.view.handle_path(handle(context))?;
        if !path.is_empty() {
            let parent = path.rsplit_once('/').map_or("", |(parent, _)| parent);
            entries.push((".".into(), handler.view.entry(inode)?));
            entries.push(("..".into(), handler.view.lookup(parent)?));
        }
        let marker = if marker.is_null() {
            None
        } else {
            Some(text(marker)?.encode_utf16().collect::<Vec<_>>())
        };
        entries.sort_by_cached_key(|(name, _)| name.encode_utf16().collect::<Vec<_>>());
        for (name, entry) in entries {
            let name: Vec<_> = name.encode_utf16().collect();
            if marker.as_ref().is_some_and(|marker| name <= *marker) {
                continue;
            }
            let size = mem::size_of::<DirInfo>() + name.len() * 2;
            let mut aligned = vec![0u64; size.div_ceil(8)];
            let item = aligned.as_mut_ptr().cast::<DirInfo>();
            unsafe {
                (*item).size = u16::try_from(size)
                    .map_err(|_| Error::Invalid("directory entry too long".into()))?;
            }
            file_info(handler, entry.inode, unsafe { &mut (*item).file })?;
            unsafe {
                ptr::copy_nonoverlapping(
                    name.as_ptr(),
                    aligned
                        .as_mut_ptr()
                        .cast::<u8>()
                        .add(mem::size_of::<DirInfo>())
                        .cast::<u16>(),
                    name.len(),
                );
            }
            if unsafe { (handler.api.add_directory)(item, buffer, length, transferred) } == 0 {
                return Ok(STATUS_SUCCESS);
            }
        }
        unsafe {
            (handler.api.add_directory)(ptr::null(), buffer, length, transferred);
        }
        Ok(STATUS_SUCCESS)
    })
}
extern "C" fn resolve_reparse_points(
    fs: Fs,
    name: Name,
    index: u32,
    last: u8,
    io: Buffer,
    buffer: Buffer,
    size: *mut usize,
) -> Status {
    call(fs, |handler| {
        Ok(unsafe {
            (handler.api.resolve_reparse)(
                fs,
                get_reparse_by_name,
                ptr::null_mut(),
                name,
                index,
                last,
                io,
                buffer,
                size,
            )
        })
    })
}
extern "C" fn get_reparse_by_name(
    fs: Fs,
    _: Context,
    name: Name,
    _: u8,
    buffer: Buffer,
    size: *mut usize,
) -> Status {
    call(fs, |handler| {
        let entry = handler.view.lookup(&path(name)?)?;
        let Some(point) = handler.view.reparse_point(entry.inode)? else {
            return Ok(STATUS_NOT_A_REPARSE_POINT);
        };
        if buffer.is_null() {
            return Ok(STATUS_SUCCESS);
        }
        let mut point = point.relocate(&handler.origin, &handler.directory)?;
        // Junction traversal needs a local volume target. Resolve internal UNC
        // targets as directory symlinks while preserving the stored link type.
        if point.tag()? == crate::reparse::JUNCTION
            && let Some(target) = point.target()?.strip_prefix(r"\??\UNC\")
        {
            point = ReparsePoint::symlink(&format!(r"\\{target}"), true)?;
        }
        copy_buffer(&point.data, buffer, size, STATUS_BUFFER_TOO_SMALL)
    })
}
extern "C" fn get_reparse_point(
    fs: Fs,
    context: Context,
    _: Name,
    buffer: Buffer,
    size: *mut usize,
) -> Status {
    call(fs, |handler| {
        let Some(point) = handler
            .view
            .reparse_point(handler.view.handle_inode(handle(context))?)?
        else {
            return Ok(STATUS_NOT_A_REPARSE_POINT);
        };
        let point = point.relocate(&handler.origin, &handler.directory)?;
        copy_buffer(&point.data, buffer, size, STATUS_BUFFER_TOO_SMALL)
    })
}
extern "C" fn set_reparse_point(
    fs: Fs,
    context: Context,
    _: Name,
    buffer: Buffer,
    size: usize,
) -> Status {
    call(fs, |handler| {
        if buffer.is_null() || size > 16 * 1024 {
            return Err(Error::Invalid("invalid reparse buffer".into()));
        }
        let inode = handler.view.handle_inode(handle(context))?;
        let entry = handler.view.entry(inode)?;
        let directory = handler
            .view
            .reparse_point(inode)?
            .map_or(entry.kind == EntryKind::Directory, |point| point.directory);
        let point = ReparsePoint::from_bytes(
            directory,
            unsafe { std::slice::from_raw_parts(buffer.cast::<u8>(), size) }.to_vec(),
        )?;
        let point = point.relocate(&handler.directory, &handler.origin)?;
        handler.view.set_reparse_point(inode, Some(point))?;
        Ok(STATUS_SUCCESS)
    })
}
extern "C" fn delete_reparse_point(
    fs: Fs,
    context: Context,
    _: Name,
    buffer: Buffer,
    size: usize,
) -> Status {
    call(fs, |handler| {
        if buffer.is_null() || size < 8 {
            return Err(Error::Invalid("invalid reparse deletion buffer".into()));
        }
        let tag = unsafe { ptr::read_unaligned(buffer.cast::<u32>()) };
        let inode = handler.view.handle_inode(handle(context))?;
        let Some(point) = handler.view.reparse_point(inode)? else {
            return Ok(STATUS_NOT_A_REPARSE_POINT);
        };
        if point.tag()? != tag {
            return Ok(STATUS_IO_REPARSE_TAG_MISMATCH);
        }
        handler.view.set_reparse_point(inode, None)?;
        Ok(STATUS_SUCCESS)
    })
}

#[derive(Clone)]
struct MountedVolume {
    identity: Vec<u16>,
    path: Vec<u16>,
}
fn mounted_directories() -> &'static Mutex<HashMap<PathBuf, MountedVolume>> {
    static MOUNTS: OnceLock<Mutex<HashMap<PathBuf, MountedVolume>>> = OnceLock::new();
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

/// Return a pinned volume root and its unique native volume path.
pub(super) fn open_mount_directory(path: &Path) -> io::Result<Option<super::MountedDirectory>> {
    if path
        .components()
        .any(|part| matches!(part, std::path::Component::Normal(_)))
    {
        return Ok(None);
    }
    let Some(root) = unc_root(path) else {
        return Ok(None);
    };
    let mounts = mounted_directories().lock();
    let Some(volume) = mounts.get(&root) else {
        return Ok(None);
    };
    let file = std::fs::OpenOptions::new()
        .access_mode(FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?;
    if device_path(&file)? != volume.identity {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "workspace mount target was replaced",
        ));
    }
    let mut native_path: Vec<u16> = r"\\?\GLOBALROOT".encode_utf16().collect();
    native_path.extend(&volume.path);
    Ok(Some(super::MountedDirectory {
        directory: file,
        path: PathBuf::from(std::ffi::OsString::from_wide(&native_path)),
    }))
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
                "WinFsp directory mount did not become ready".into(),
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
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
