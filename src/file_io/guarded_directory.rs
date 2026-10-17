//! Directory-relative mutations on Unix; pinned non-reparse ancestors on Windows.
#[cfg(any(windows, test))]
use std::fs;
use std::{
    ffi::{OsStr, OsString},
    fs::File,
    io,
    path::{Component, Path, PathBuf},
};

pub(super) struct GuardedDirectory {
    #[cfg(windows)]
    path: PathBuf,
    #[cfg(unix)]
    directory: File,
    #[cfg(windows)]
    _ancestors: Vec<File>,
}

fn single_name(name: &OsStr) -> io::Result<()> {
    if matches!(
        Path::new(name).components().next(),
        Some(Component::Normal(_))
    ) && Path::new(name).components().count() == 1
    {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid relative filename",
        ))
    }
}

/// This broker supports local volumes only. UNC and device namespaces can
/// connect to pipes or remote servers before handle validation can run.
pub(super) fn local_path(path: &Path) -> io::Result<()> {
    #[cfg(windows)]
    if let Some(Component::Prefix(prefix)) = path.components().next()
        && !matches!(
            prefix.kind(),
            std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_)
        )
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "remote and device filesystem namespaces are not supported",
        ));
    }
    #[cfg(not(windows))]
    let _ = path;
    Ok(())
}

impl GuardedDirectory {
    pub(super) fn open(path: &Path, create: bool) -> io::Result<Self> {
        Self::open_with(path, create, |_| Ok(()))
    }

    pub(super) fn open_with(
        path: &Path,
        create: bool,
        mut check_create: impl FnMut(&Path) -> io::Result<()>,
    ) -> io::Result<Self> {
        if !path.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "directory must be absolute",
            ));
        }
        local_path(path)?;
        if path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "directory path must be normalized",
            ));
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
            use windows_sys::Win32::Storage::FileSystem::{
                FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
                FILE_FLAG_OPEN_REPARSE_POINT, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES,
                FILE_SHARE_READ, FILE_SHARE_WRITE,
            };
            let mut ancestors = Vec::new();
            // Windows canonical paths may have a final, non-absolute drive
            // prefix ancestor (\\?\C:). It is not an openable directory.
            for ancestor in path
                .ancestors()
                .filter(|path| path.is_absolute())
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
            {
                if create {
                    match fs::symlink_metadata(ancestor) {
                        Ok(_) => {}
                        Err(err) if err.kind() == io::ErrorKind::NotFound => {
                            check_create(ancestor)?;
                            match fs::create_dir(ancestor) {
                                Ok(()) => {}
                                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
                                Err(err) => return Err(err),
                            }
                        }
                        Err(err) => return Err(err),
                    }
                }
                // FILE_LIST_DIRECTORY participates in Windows share-access
                // accounting; access_mode(0) does not block rename. Contents
                // are never enumerated. Omitted SHARE_DELETE pins the path.
                let handle = fs::OpenOptions::new()
                    .access_mode(FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES)
                    .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                    .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
                    .open(ancestor)?;
                let metadata = handle.metadata()?;
                if !metadata.is_dir()
                    || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
                {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "directory reparse point is forbidden",
                    ));
                }
                ancestors.push(handle);
            }
            Ok(Self {
                path: path.to_path_buf(),
                _ancestors: ancestors,
            })
        }
        #[cfg(unix)]
        {
            use std::os::{
                fd::{AsRawFd, FromRawFd},
                unix::ffi::OsStrExt,
            };
            let root = std::ffi::CString::new("/").unwrap();
            // Android app UIDs may traverse /data ancestors without listing them.
            // O_PATH pins these ancestors without requesting read-directory access.
            #[cfg(target_os = "android")]
            let ancestor_flags = libc::O_PATH;
            #[cfg(not(target_os = "android"))]
            let ancestor_flags = libc::O_RDONLY;
            let raw = unsafe {
                libc::open(
                    root.as_ptr(),
                    ancestor_flags | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if raw < 0 {
                return Err(io::Error::last_os_error());
            }
            let mut directory = unsafe { File::from_raw_fd(raw) };
            let mut current = PathBuf::from("/");
            for component in path.components() {
                let Component::Normal(name) = component else {
                    continue;
                };
                let name = std::ffi::CString::new(name.as_bytes()).map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidInput, "invalid directory component")
                })?;
                current.push(OsStr::from_bytes(name.as_bytes()));
                let mut raw = unsafe {
                    libc::openat(
                        directory.as_raw_fd(),
                        name.as_ptr(),
                        ancestor_flags | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                    )
                };
                if raw < 0 && create && io::Error::last_os_error().kind() == io::ErrorKind::NotFound
                {
                    check_create(&current)?;
                    if unsafe { libc::mkdirat(directory.as_raw_fd(), name.as_ptr(), 0o777) } < 0 {
                        let error = io::Error::last_os_error();
                        if error.kind() != io::ErrorKind::AlreadyExists {
                            return Err(error);
                        }
                    }
                    raw = unsafe {
                        libc::openat(
                            directory.as_raw_fd(),
                            name.as_ptr(),
                            ancestor_flags | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                        )
                    };
                }
                if raw < 0 {
                    return Err(io::Error::last_os_error());
                }
                directory = unsafe { File::from_raw_fd(raw) };
            }
            #[cfg(target_os = "android")]
            {
                let raw = unsafe {
                    libc::openat(
                        directory.as_raw_fd(),
                        c".".as_ptr(),
                        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                    )
                };
                if raw < 0 {
                    return Err(io::Error::last_os_error());
                }
                directory = unsafe { File::from_raw_fd(raw) };
            }
            Ok(Self { directory })
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (path, create, check_create);
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "guarded directories unavailable",
            ))
        }
    }

    pub(super) fn handle(&self) -> &File {
        #[cfg(unix)]
        {
            &self.directory
        }
        #[cfg(windows)]
        {
            self._ancestors
                .last()
                .expect("absolute directory has an ancestor")
        }
    }

    /// Enumerate the pinned directory itself, never reopen a path that Unix
    /// can rename after authorization. Only names are returned to callers.
    pub(super) fn names(&self) -> io::Result<Vec<OsString>> {
        #[cfg(windows)]
        {
            let mut names = Vec::new();
            for entry in fs::read_dir(&self.path)? {
                names.push(entry?.file_name());
            }
            Ok(names)
        }
        #[cfg(unix)]
        {
            use std::{
                ffi::CStr,
                os::{fd::AsRawFd, unix::ffi::OsStrExt},
            };
            // Opening '.' relative to the pinned FD gives an independent
            // enumeration offset without reopening the original path.
            let descriptor = unsafe {
                libc::openat(
                    self.directory.as_raw_fd(),
                    c".".as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if descriptor < 0 {
                return Err(io::Error::last_os_error());
            }
            let stream = unsafe { libc::fdopendir(descriptor) };
            if stream.is_null() {
                unsafe {
                    libc::close(descriptor);
                }
                return Err(io::Error::last_os_error());
            }
            struct DirectoryStream(*mut libc::DIR);
            impl Drop for DirectoryStream {
                fn drop(&mut self) {
                    unsafe {
                        libc::closedir(self.0);
                    }
                }
            }
            let stream = DirectoryStream(stream);
            let mut names = Vec::new();
            loop {
                set_directory_errno(0)?;
                let entry = unsafe { libc::readdir(stream.0) };
                if entry.is_null() {
                    let error = io::Error::last_os_error();
                    if error.raw_os_error() != Some(0) {
                        return Err(error);
                    }
                    break;
                }
                let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
                if name == b"." || name == b".." {
                    continue;
                }
                names.push(OsStr::from_bytes(name).to_owned());
            }
            Ok(names)
        }
        #[cfg(not(any(unix, windows)))]
        {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "guarded enumeration unavailable",
            ))
        }
    }

    pub(super) fn create_new(&self, name: &OsStr) -> io::Result<File> {
        single_name(name)?;
        #[cfg(windows)]
        {
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(self.path.join(name))
        }
        #[cfg(unix)]
        {
            use std::os::fd::{AsRawFd, FromRawFd};
            let name = c_name(name)?;
            let raw = unsafe {
                libc::openat(
                    self.directory.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_WRONLY
                        | libc::O_CREAT
                        | libc::O_EXCL
                        | libc::O_NOFOLLOW
                        | libc::O_CLOEXEC,
                    0o666,
                )
            };
            if raw < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(unsafe { File::from_raw_fd(raw) })
            }
        }
        #[cfg(not(any(unix, windows)))]
        {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "guarded writes unavailable",
            ))
        }
    }

    pub(super) fn rename(&self, source: &OsStr, target: &OsStr) -> io::Result<()> {
        single_name(source)?;
        single_name(target)?;
        #[cfg(windows)]
        {
            fs::rename(self.path.join(source), self.path.join(target))
        }
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let (source, target) = (c_name(source)?, c_name(target)?);
            syscall_result(unsafe {
                libc::renameat(
                    self.directory.as_raw_fd(),
                    source.as_ptr(),
                    self.directory.as_raw_fd(),
                    target.as_ptr(),
                )
            })
        }
        #[cfg(not(any(unix, windows)))]
        {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "guarded rename unavailable",
            ))
        }
    }

    pub(super) fn publish_new(&self, source: &OsStr, target: &OsStr) -> io::Result<()> {
        #[cfg(target_os = "android")]
        {
            use std::os::fd::AsRawFd;
            single_name(source)?;
            single_name(target)?;
            let (source, target) = (c_name(source)?, c_name(target)?);
            // Android SELinux denies hardlinks even in app-private storage.
            // RENAME_NOREPLACE keeps the atomic create/freshness guarantee;
            // unsupported kernels fail closed instead of overwriting a racer.
            syscall_result(unsafe {
                libc::syscall(
                    libc::SYS_renameat2,
                    self.directory.as_raw_fd(),
                    source.as_ptr(),
                    self.directory.as_raw_fd(),
                    target.as_ptr(),
                    1u32,
                )
            } as libc::c_int)
        }
        #[cfg(not(target_os = "android"))]
        {
            self.link(source, target)?;
            self.remove(source)
        }
    }

    #[cfg(not(target_os = "android"))]
    fn link(&self, source: &OsStr, target: &OsStr) -> io::Result<()> {
        single_name(source)?;
        single_name(target)?;
        #[cfg(windows)]
        {
            fs::hard_link(self.path.join(source), self.path.join(target))
        }
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let (source, target) = (c_name(source)?, c_name(target)?);
            syscall_result(unsafe {
                libc::linkat(
                    self.directory.as_raw_fd(),
                    source.as_ptr(),
                    self.directory.as_raw_fd(),
                    target.as_ptr(),
                    0,
                )
            })
        }
        #[cfg(not(any(unix, windows)))]
        {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "guarded link unavailable",
            ))
        }
    }

    pub(super) fn remove(&self, name: &OsStr) -> io::Result<()> {
        single_name(name)?;
        #[cfg(windows)]
        {
            fs::remove_file(self.path.join(name))
        }
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let name = c_name(name)?;
            syscall_result(unsafe { libc::unlinkat(self.directory.as_raw_fd(), name.as_ptr(), 0) })
        }
        #[cfg(not(any(unix, windows)))]
        {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "guarded removal unavailable",
            ))
        }
    }

    pub(super) fn sync(&self) -> io::Result<()> {
        #[cfg(unix)]
        {
            self.directory.sync_all()
        }
        #[cfg(not(unix))]
        {
            Ok(())
        }
    }
}

#[cfg(unix)]
fn set_directory_errno(value: libc::c_int) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    unsafe {
        *libc::__errno_location() = value;
    }
    #[cfg(any(
        target_vendor = "apple",
        target_os = "freebsd",
        target_os = "dragonfly"
    ))]
    unsafe {
        *libc::__error() = value;
    }
    #[cfg(any(target_os = "android", target_os = "netbsd", target_os = "openbsd"))]
    unsafe {
        *libc::__errno() = value;
    }
    #[cfg(any(target_os = "solaris", target_os = "illumos"))]
    unsafe {
        *libc::___errno() = value;
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "android",
        target_vendor = "apple",
        target_os = "freebsd",
        target_os = "dragonfly",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "solaris",
        target_os = "illumos"
    )))]
    {
        let _ = value;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "directory enumeration error reporting is unsupported",
        ))
    }
    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_vendor = "apple",
        target_os = "freebsd",
        target_os = "dragonfly",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "solaris",
        target_os = "illumos"
    ))]
    Ok(())
}

#[cfg(unix)]
fn c_name(name: &OsStr) -> io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid filename"))
}
#[cfg(unix)]
fn syscall_result(result: libc::c_int) -> io::Result<()> {
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(windows)]
    #[test]
    fn guarded_parent_cannot_be_renamed_while_mutating() {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("parent");
        fs::create_dir(&parent).unwrap();
        let guard = GuardedDirectory::open(&parent.canonicalize().unwrap(), false).unwrap();
        assert!(fs::rename(&parent, dir.path().join("moved")).is_err());
        drop(guard);
        fs::rename(&parent, dir.path().join("moved")).unwrap();
    }
    #[cfg(windows)]
    #[test]
    fn canonical_drive_prefix_is_not_opened_or_created_as_directory() {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().canonicalize().unwrap();
        let nested = parent.join("nested/child");
        let guard = GuardedDirectory::open(&nested, true).unwrap();
        let mut file = guard.create_new(OsStr::new("new.txt")).unwrap();
        std::io::Write::write_all(&mut file, b"created in the pinned directory").unwrap();
        drop(file);
        assert!(fs::rename(parent.join("nested"), parent.join("moved")).is_err());
        assert!(nested.join("new.txt").is_file());
    }
    #[cfg(windows)]
    #[test]
    fn remote_and_device_namespaces_are_rejected_before_opening() {
        for path in [
            r"\\server\share\directory",
            r"\\?\UNC\server\share\directory",
            r"\\.\pipe\untrusted",
            r"\\?\GLOBALROOT\Device\HarddiskVolume1\directory",
        ] {
            assert!(GuardedDirectory::open(Path::new(path), false).is_err());
        }
    }
    #[cfg(unix)]
    #[test]
    fn renamed_parent_never_redirects_relative_write_into_a_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let parent = dir.path().join("parent");
        fs::create_dir(&parent).unwrap();
        let guard = GuardedDirectory::open(&parent.canonicalize().unwrap(), false).unwrap();
        fs::rename(&parent, dir.path().join("moved")).unwrap();
        std::os::unix::fs::symlink(outside.path(), &parent).unwrap();
        drop(guard.create_new(OsStr::new("new")).unwrap());
        assert!(dir.path().join("moved/new").exists());
        assert!(!outside.path().join("new").exists());
        let names = guard.names().unwrap();
        assert_eq!(names, [OsString::from("new")]);
    }
    #[test]
    fn missing_directory_checks_every_created_component_before_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let allowed = root.join("new/child");
        let result = GuardedDirectory::open_with(&allowed, true, |path| {
            if path == allowed {
                Ok(())
            } else {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "parent is ungranted",
                ))
            }
        });
        assert!(result.is_err());
        assert!(!root.join("new").exists());
    }
}
