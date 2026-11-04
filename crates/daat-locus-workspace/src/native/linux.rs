use super::{
    Mount,
    view::{READ, SHARE_ALL, View, WRITE},
};
use crate::{Entry, EntryKind, Error, Result, Workspace};
use fuser::*;
use parking_lot::Mutex;
use std::{
    collections::HashMap,
    ffi::{CString, OsStr},
    io::{self, Write},
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::Path,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub(super) fn available() -> Result<bool> {
    match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/fuse")
    {
        Ok(_) => {}
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
            ) =>
        {
            return Ok(false);
        }
        Err(error) => return Err(error.into()),
    }
    Ok([
        "/usr/bin/fusermount3",
        "/bin/fusermount3",
        "/usr/bin/fusermount",
        "/bin/fusermount",
    ]
    .iter()
    .any(|path| Path::new(path).is_file()))
}

pub(super) fn mount(workspace: Workspace, path: &Path) -> Result<Mount> {
    if !available()? {
        return Err(Error::Unsupported(
            "FUSE device or mount helper is unavailable".into(),
        ));
    }
    let view = Arc::new(View::new(workspace)?);
    let filesystem = FilesystemAdapter {
        view: view.clone(),
        directories: Mutex::new(HashMap::new()),
        uid: unsafe { libc::getuid() },
        gid: unsafe { libc::getgid() },
    };
    let mut config = Config::default();
    config.mount_options = vec![
        MountOption::FSName("daat-locus-workspace".into()),
        MountOption::RW,
        MountOption::NoDev,
        MountOption::NoSuid,
        MountOption::DefaultPermissions,
    ];
    // Owner-only access is sufficient for all account Actors. AutoUnmount is
    // deliberately omitted: fuser requires allow_other/allow_root for that flag.
    let mut session = fuser::Session::new(filesystem, path, &config)?;
    let mut unmounter = session.unmount_callable();
    let guard = std::thread::Builder::new()
        .name("workspace-fuse".into())
        .spawn(move || session.run())?;
    let abort = (|| -> io::Result<std::fs::File> {
        // fusectl labels connections with the mounted superblock's minor
        // device number and grants its owner write access to abort. Hold the
        // control fd so later numeric ID reuse cannot target another mount.
        let device = std::fs::metadata(path)?.dev();
        let connection = libc::minor(device);
        std::fs::OpenOptions::new()
            .write(true)
            .open(format!("/sys/fs/fuse/connections/{connection}/abort"))
    })();
    let abort = match abort {
        Ok(abort) => abort,
        Err(error) => {
            let unmount = unmounter.unmount();
            let _ = guard.join();
            unmount?;
            return Err(Error::Unsupported(format!(
                "FUSE connection cleanup control is unavailable: {error}"
            )));
        }
    };
    Ok(Mount {
        directory: path.to_owned(),
        _session: Session {
            guard: Some(guard),
            unmounter,
            abort,
            view,
            directory: path.to_owned(),
        },
    })
}

pub(super) struct Session {
    guard: Option<std::thread::JoinHandle<io::Result<()>>>,
    unmounter: fuser::SessionUnmounter,
    abort: std::fs::File,
    view: Arc<View>,
    directory: std::path::PathBuf,
}

impl Session {
    pub(super) fn retire(&mut self) -> Result<()> {
        self.view.retire()?;
        let result = (|| -> Result<()> {
            let helper = [
                "/usr/bin/fusermount3",
                "/bin/fusermount3",
                "/usr/bin/fusermount",
                "/bin/fusermount",
            ]
            .into_iter()
            .find(|path| Path::new(path).is_file())
            .ok_or_else(|| Error::Unsupported("FUSE unmount helper is unavailable".into()))?;
            // A normal unmount refuses kernel-held cwd/directory references.
            // fuser's general shutdown uses lazy unmount and cannot prove idleness.
            if !std::process::Command::new(helper)
                .args(["-u", "--"])
                .arg(&self.directory)
                .status()?
                .success()
            {
                return Err(Error::Conflict(
                    "workspace projection is busy or cannot be unmounted".into(),
                ));
            }
            Ok(())
        })();
        if result.is_err() {
            self.view.resume();
        }
        result
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // A user application may retain an fd or cwd in a branch. A lazy
        // unmount alone leaves the FUSE thread alive in that case. Abort only
        // this owned connection, then join it before releasing its backing DB.
        let _ = self.abort.write_all(b"1");
        let _ = self.unmounter.unmount();
        if let Some(guard) = self.guard.take() {
            let _ = guard.join();
        }
    }
}

struct FilesystemAdapter {
    view: Arc<View>,
    directories: Mutex<HashMap<u64, Vec<(String, Entry)>>>,
    uid: u32,
    gid: u32,
}

fn errno(error: Error) -> Errno {
    match error {
        Error::Io(error) => Errno::from_i32(error.raw_os_error().unwrap_or(match error.kind() {
            io::ErrorKind::NotFound => libc::ENOENT,
            io::ErrorKind::AlreadyExists => libc::EEXIST,
            io::ErrorKind::PermissionDenied => libc::EACCES,
            io::ErrorKind::DirectoryNotEmpty => libc::ENOTEMPTY,
            io::ErrorKind::NotADirectory => libc::ENOTDIR,
            io::ErrorKind::WouldBlock => libc::EAGAIN,
            _ => libc::EIO,
        })),
        Error::Invalid(_) => Errno::EINVAL,
        Error::Authorization(_) => Errno::EACCES,
        Error::Unsupported(_) => Errno::EOPNOTSUPP,
        Error::Conflict(_) => Errno::EBUSY,
        _ => Errno::EIO,
    }
}

fn name(value: &OsStr) -> Result<&str> {
    value
        .to_str()
        .ok_or_else(|| Error::Invalid("workspace names must be Unicode".into()))
}

fn kind(kind: EntryKind) -> FileType {
    match kind {
        EntryKind::File => FileType::RegularFile,
        EntryKind::Directory => FileType::Directory,
        EntryKind::Symlink => FileType::Symlink,
    }
}

impl FilesystemAdapter {
    fn inode(&self, ino: INodeNo) -> u64 {
        if ino.0 == 1 {
            self.view.root_inode()
        } else {
            ino.0
        }
    }
    fn native_inode(&self, inode: u64) -> INodeNo {
        INodeNo(if inode == self.view.root_inode() {
            1
        } else {
            inode
        })
    }
    fn attr(&self, entry: &Entry) -> FileAttr {
        let time = UNIX_EPOCH + Duration::from_nanos(entry.modified_ns.max(0) as u64);
        FileAttr {
            ino: self.native_inode(entry.inode),
            size: entry.size,
            blocks: entry.size.div_ceil(512),
            atime: time,
            mtime: time,
            ctime: time,
            crtime: time,
            kind: kind(entry.kind),
            perm: entry.mode as u16,
            nlink: if entry.kind == EntryKind::Directory {
                2
            } else {
                1
            },
            uid: self.uid,
            gid: self.gid,
            rdev: 0,
            blksize: 4096,
            flags: 0,
        }
    }
    fn child(&self, parent: INodeNo, child: &OsStr) -> Result<String> {
        self.view.child(self.inode(parent), name(child)?)
    }
    fn open_file(&self, ino: INodeNo, flags: i32) -> Result<FileHandle> {
        let inode = self.inode(ino);
        let access = match flags & libc::O_ACCMODE {
            libc::O_WRONLY => WRITE,
            libc::O_RDWR => READ | WRITE,
            _ => READ,
        };
        let handle = self.view.open(
            &self.view.inode_path(inode)?,
            access,
            SHARE_ALL,
            flags & libc::O_APPEND != 0,
        )?;
        if flags & libc::O_TRUNC != 0 {
            if access & WRITE == 0 {
                self.view.close(handle);
                return Err(io::Error::from(io::ErrorKind::PermissionDenied).into());
            }
            if let Err(error) = self.view.truncate(inode, 0) {
                self.view.close(handle);
                return Err(error);
            }
        }
        Ok(FileHandle(handle))
    }
}

impl Filesystem for FilesystemAdapter {
    fn lookup(&self, _: &Request, parent: INodeNo, child: &OsStr, reply: ReplyEntry) {
        match self
            .child(parent, child)
            .and_then(|path| self.view.lookup(&path))
        {
            Ok(entry) => reply.entry(&Duration::ZERO, &self.attr(&entry), Generation(0)),
            Err(error) => reply.error(errno(error)),
        }
    }
    fn getattr(&self, _: &Request, ino: INodeNo, _: Option<FileHandle>, reply: ReplyAttr) {
        match self.view.entry(self.inode(ino)) {
            Ok(entry) => reply.attr(&Duration::ZERO, &self.attr(&entry)),
            Err(error) => reply.error(errno(error)),
        }
    }
    fn setattr(
        &self,
        _: &Request,
        ino: INodeNo,
        mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
        size: Option<u64>,
        _: Option<TimeOrNow>,
        mtime: Option<TimeOrNow>,
        _: Option<SystemTime>,
        _: Option<FileHandle>,
        _: Option<SystemTime>,
        _: Option<SystemTime>,
        _: Option<SystemTime>,
        _: Option<BsdFileFlags>,
        reply: ReplyAttr,
    ) {
        let result = (|| -> Result<Entry> {
            if uid.is_some_and(|uid| uid != self.uid) || gid.is_some_and(|gid| gid != self.gid) {
                return Err(io::Error::from(io::ErrorKind::PermissionDenied).into());
            }
            let inode = self.inode(ino);
            if let Some(size) = size {
                self.view.truncate(inode, size)?;
            }
            let modified = mtime
                .map(|time| match time {
                    TimeOrNow::SpecificTime(time) => time,
                    TimeOrNow::Now => SystemTime::now(),
                })
                .map(|time| {
                    time.duration_since(UNIX_EPOCH)
                        .map_or(0, |time| time.as_nanos().min(i64::MAX as u128) as i64)
                });
            if mode.is_some() || modified.is_some() {
                self.view.set_metadata(inode, mode, modified)?;
            }
            self.view.entry(inode)
        })();
        match result {
            Ok(entry) => reply.attr(&Duration::ZERO, &self.attr(&entry)),
            Err(error) => reply.error(errno(error)),
        }
    }
    fn mknod(
        &self,
        _: &Request,
        parent: INodeNo,
        child: &OsStr,
        mode: u32,
        umask: u32,
        _: u32,
        reply: ReplyEntry,
    ) {
        let result = if mode & libc::S_IFMT != libc::S_IFREG {
            Err(Error::Unsupported("special filesystem nodes".into()))
        } else {
            self.child(parent, child)
                .and_then(|path| self.view.create(&path, false, mode & !umask))
        };
        match result {
            Ok(entry) => reply.entry(&Duration::ZERO, &self.attr(&entry), Generation(0)),
            Err(error) => reply.error(errno(error)),
        }
    }
    fn mkdir(
        &self,
        _: &Request,
        parent: INodeNo,
        child: &OsStr,
        mode: u32,
        umask: u32,
        reply: ReplyEntry,
    ) {
        match self
            .child(parent, child)
            .and_then(|path| self.view.create(&path, true, mode & !umask))
        {
            Ok(entry) => reply.entry(&Duration::ZERO, &self.attr(&entry), Generation(0)),
            Err(error) => reply.error(errno(error)),
        }
    }
    fn unlink(&self, _: &Request, parent: INodeNo, child: &OsStr, reply: ReplyEmpty) {
        match self
            .child(parent, child)
            .and_then(|path| self.view.remove(&path, false, None))
        {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(errno(error)),
        }
    }
    fn rmdir(&self, _: &Request, parent: INodeNo, child: &OsStr, reply: ReplyEmpty) {
        match self
            .child(parent, child)
            .and_then(|path| self.view.remove(&path, true, None))
        {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(errno(error)),
        }
    }
    fn rename(
        &self,
        _: &Request,
        parent: INodeNo,
        child: &OsStr,
        newparent: INodeNo,
        newname: &OsStr,
        flags: RenameFlags,
        reply: ReplyEmpty,
    ) {
        let result = (|| -> Result<()> {
            if flags.bits() & !1 != 0 {
                return Err(Error::Unsupported("rename exchange/whiteout".into()));
            }
            self.view.rename(
                &self.child(parent, child)?,
                &self.child(newparent, newname)?,
                flags.bits() & 1 == 0,
                None,
            )
        })();
        match result {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(errno(error)),
        }
    }
    fn open(&self, _: &Request, ino: INodeNo, flags: OpenFlags, reply: ReplyOpen) {
        match self.open_file(ino, flags.0) {
            Ok(handle) => reply.opened(handle, FopenFlags::empty()),
            Err(error) => reply.error(errno(error)),
        }
    }
    fn read(
        &self,
        _: &Request,
        _: INodeNo,
        handle: FileHandle,
        offset: u64,
        size: u32,
        _: OpenFlags,
        _: Option<LockOwner>,
        reply: ReplyData,
    ) {
        match self.view.read(handle.0, offset, size as usize) {
            Ok(bytes) => reply.data(&bytes),
            Err(error) => reply.error(errno(error)),
        }
    }
    fn write(
        &self,
        _: &Request,
        _: INodeNo,
        handle: FileHandle,
        offset: u64,
        data: &[u8],
        _: WriteFlags,
        _: OpenFlags,
        _: Option<LockOwner>,
        reply: ReplyWrite,
    ) {
        match self.view.write(handle.0, offset, data, false) {
            Ok(length) => reply.written(length as u32),
            Err(error) => reply.error(errno(error)),
        }
    }
    fn flush(&self, _: &Request, _: INodeNo, _: FileHandle, _: LockOwner, reply: ReplyEmpty) {
        reply.ok();
    }
    fn fsync(&self, _: &Request, _: INodeNo, _: FileHandle, _: bool, reply: ReplyEmpty) {
        reply.ok();
    }
    fn release(
        &self,
        _: &Request,
        _: INodeNo,
        handle: FileHandle,
        _: OpenFlags,
        _: Option<LockOwner>,
        _: bool,
        reply: ReplyEmpty,
    ) {
        self.view.close(handle.0);
        reply.ok();
    }
    fn opendir(&self, _: &Request, ino: INodeNo, _: OpenFlags, reply: ReplyOpen) {
        let result = (|| -> Result<FileHandle> {
            let inode = self.inode(ino);
            let current = self.view.entry(inode)?;
            let path = self.view.inode_path(inode)?;
            let parent_path = path.rsplit_once('/').map_or("", |(parent, _)| parent);
            let parent = self.view.lookup(parent_path)?;
            let mut entries = vec![(".".into(), current), ("..".into(), parent)];
            entries.extend(self.view.read_dir(inode)?);
            let handle = self.view.open(&path, READ, SHARE_ALL, false)?;
            self.directories.lock().insert(handle, entries);
            Ok(FileHandle(handle))
        })();
        match result {
            Ok(handle) => reply.opened(handle, FopenFlags::empty()),
            Err(error) => reply.error(errno(error)),
        }
    }
    fn readdir(
        &self,
        _: &Request,
        _: INodeNo,
        handle: FileHandle,
        offset: u64,
        mut reply: ReplyDirectory,
    ) {
        let entries = self.directories.lock();
        let Some(entries) = entries.get(&handle.0) else {
            reply.error(Errno::EBADF);
            return;
        };
        for (index, (name, entry)) in entries.iter().enumerate().skip(offset as usize) {
            if reply.add(
                self.native_inode(entry.inode),
                index as u64 + 1,
                kind(entry.kind),
                name,
            ) {
                break;
            }
        }
        reply.ok();
    }
    fn releasedir(
        &self,
        _: &Request,
        _: INodeNo,
        handle: FileHandle,
        _: OpenFlags,
        reply: ReplyEmpty,
    ) {
        self.directories.lock().remove(&handle.0);
        self.view.close(handle.0);
        reply.ok();
    }
    fn create(
        &self,
        _: &Request,
        parent: INodeNo,
        child: &OsStr,
        mode: u32,
        umask: u32,
        flags: i32,
        reply: ReplyCreate,
    ) {
        let result = (|| -> Result<(Entry, FileHandle)> {
            let entry = self
                .view
                .create(&self.child(parent, child)?, false, mode & !umask)?;
            let handle = self.open_file(self.native_inode(entry.inode), flags)?;
            Ok((entry, handle))
        })();
        match result {
            Ok((entry, handle)) => reply.created(
                &Duration::ZERO,
                &self.attr(&entry),
                Generation(0),
                handle,
                FopenFlags::empty(),
            ),
            Err(error) => reply.error(errno(error)),
        }
    }
    fn readlink(&self, _: &Request, ino: INodeNo, reply: ReplyData) {
        match self.view.read_link(self.inode(ino)) {
            Ok(target) => reply.data(target.as_bytes()),
            Err(error) => reply.error(errno(error)),
        }
    }
    fn symlink(
        &self,
        _: &Request,
        parent: INodeNo,
        child: &OsStr,
        target: &Path,
        reply: ReplyEntry,
    ) {
        let result = (|| -> Result<Entry> {
            self.view
                .symlink(&self.child(parent, child)?, name(target.as_os_str())?)
        })();
        match result {
            Ok(entry) => reply.entry(&Duration::ZERO, &self.attr(&entry), Generation(0)),
            Err(error) => reply.error(errno(error)),
        }
    }
    fn statfs(&self, _: &Request, _: INodeNo, reply: ReplyStatfs) {
        let result = (|| -> Result<libc::statvfs> {
            let path = CString::new(self.view.backing_path().as_os_str().as_bytes())
                .map_err(|_| Error::Invalid("storage path contains NUL".into()))?;
            let mut stat = std::mem::MaybeUninit::uninit();
            if unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
                return Err(io::Error::last_os_error().into());
            }
            Ok(unsafe { stat.assume_init() })
        })();
        match result {
            Ok(stat) => reply.statfs(
                stat.f_blocks,
                stat.f_bfree,
                stat.f_bavail,
                stat.f_files,
                stat.f_ffree,
                stat.f_bsize as u32,
                stat.f_namemax as u32,
                stat.f_frsize as u32,
            ),
            Err(error) => reply.error(errno(error)),
        }
    }
}
