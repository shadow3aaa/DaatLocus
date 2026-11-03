use crate::{Error, Result};
use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::fs::{Dir, OpenOptions};
use sha2::{Digest, Sha256};
use std::{
    hash::{Hash, Hasher},
    io::{self, Read},
    path::{Component, Path, PathBuf},
    sync::Arc,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
}

/// Called for each host access, including descendants, temporary files and
/// deletion targets. An App supplies its current common sandbox policy here.
pub type Authorize = Arc<dyn Fn(&Path, Access) -> Result<()> + Send + Sync>;

#[derive(Clone)]
pub struct Directory {
    pub(crate) path: PathBuf,
    pub(crate) dir: Arc<Dir>,
    identity: Arc<same_file::Handle>,
    authorize: Authorize,
}

impl Directory {
    pub fn open(path: &Path, authorize: Authorize) -> Result<Self> {
        authorize(path, Access::Read)?;
        let path = path.canonicalize()?;
        authorize(&path, Access::Read)?;
        let dir = Dir::open_ambient_dir(&path, cap_std::ambient_authority())?;
        let identity = same_file::Handle::from_file(dir.try_clone()?.into_std_file())?;
        let result = Self {
            path,
            dir: Arc::new(dir),
            identity: Arc::new(identity),
            authorize,
        };
        result.check_binding()?;
        Ok(result)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn binding(&self) -> String {
        struct Fingerprint(Sha256);
        impl Hasher for Fingerprint {
            fn write(&mut self, bytes: &[u8]) {
                self.0.update(bytes);
            }
            fn finish(&self) -> u64 {
                unreachable!("identity hash uses its complete digest")
            }
        }
        let mut fingerprint = Fingerprint(Sha256::new());
        self.identity.hash(&mut fingerprint);
        hex::encode(fingerprint.0.finalize())
    }

    pub fn check_binding(&self) -> Result<()> {
        let current = same_file::Handle::from_path(&self.path)?;
        if current != *self.identity || self.path.canonicalize()? != self.path {
            return Err(Error::Conflict(
                "source directory was moved or replaced".into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn access(&self, relative: &str, access: Access) -> Result<PathBuf> {
        self.check_binding()?;
        let relative = relative_path(relative)?;
        (self.authorize)(&self.path.join(&relative), access)?;
        Ok(relative)
    }

    pub(crate) fn metadata(&self, path: &str) -> Result<cap_std::fs::Metadata> {
        self.access(path, Access::Read)?;
        if path.is_empty() {
            return Ok(self.dir.dir_metadata()?);
        }
        let (directory, name) = self.parent(path)?;
        Ok(directory.symlink_metadata(name)?)
    }

    fn open_directory(&self, path: &Path) -> Result<Dir> {
        let mut directory = self.dir.try_clone()?;
        for component in path.components() {
            if let Component::Normal(name) = component {
                directory = directory.open_dir_nofollow(name)?;
            } else {
                return Err(Error::Invalid("expected a relative directory".into()));
            }
        }
        Ok(directory)
    }

    fn parent(&self, path: &str) -> Result<(Dir, std::ffi::OsString)> {
        let relative = relative_path(path)?;
        let name = relative
            .file_name()
            .ok_or_else(|| Error::Invalid("cannot replace the root".into()))?
            .to_owned();
        Ok((
            self.open_directory(relative.parent().unwrap_or(Path::new("")))?,
            name,
        ))
    }

    pub(crate) fn names(&self, path: &str) -> Result<Vec<String>> {
        let relative = self.access(path, Access::Read)?;
        let directory = self.open_directory(&relative)?;
        let mut names = directory
            .entries()?
            .map(|entry| {
                let name = entry?
                    .file_name()
                    .into_string()
                    .map_err(|_| Error::Invalid("workspace names must be Unicode".into()))?;
                validate_name(&name)?;
                Ok(name)
            })
            .collect::<Result<Vec<_>>>()?;
        names.sort();
        Ok(names)
    }

    pub(crate) fn read_chunks(
        &self,
        path: &str,
        mut consume: impl FnMut(&[u8]) -> Result<()>,
    ) -> Result<u64> {
        self.access(path, Access::Read)?;
        let (directory, name) = self.parent(path)?;
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No);
        let mut file = directory.open_with(&name, &options)?;
        let before = file.metadata()?;
        if !before.is_file() {
            return Err(Error::Invalid("expected a regular file".into()));
        }
        let mut buffer = vec![0; 64 * 1024];
        let mut length = 0;
        loop {
            self.access(path, Access::Read)?;
            let mut count = 0;
            while count < buffer.len() {
                match file.read(&mut buffer[count..]) {
                    Ok(0) => break,
                    Ok(read) => count += read,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(error.into()),
                }
            }
            if count == 0 {
                break;
            }
            consume(&buffer[..count])?;
            length += count as u64;
        }
        let after = file.metadata()?;
        let first = same_file::Handle::from_file(file.into_std())?;
        let second =
            same_file::Handle::from_file(directory.open_with(&name, &options)?.into_std())?;
        if first != second
            || before.len() != after.len()
            || before.modified()? != after.modified()?
        {
            return Err(Error::Conflict(format!(
                "source changed while reading {path}"
            )));
        }
        Ok(length)
    }

    #[cfg(not(windows))]
    pub(crate) fn read_link(&self, path: &str) -> Result<String> {
        self.access(path, Access::Read)?;
        let (directory, name) = self.parent(path)?;
        directory
            .read_link_contents(name)?
            .into_os_string()
            .into_string()
            .map_err(|_| Error::Invalid("workspace symlink targets must be Unicode".into()))
    }

    #[cfg(windows)]
    pub(crate) fn read_reparse(&self, path: &str) -> Result<crate::reparse::ReparsePoint> {
        use cap_std::fs::{MetadataExt, OpenOptionsExt};
        use std::os::windows::io::AsRawHandle;
        self.access(path, Access::Read)?;
        let (directory, name) = self.parent(path)?;
        let mut options = OpenOptions::new();
        options
            .access_mode(0x80)
            .share_mode(1)
            .custom_flags(0x0220_0000)
            .follow(FollowSymlinks::No);
        let file = directory.open_with(name, &options)?;
        let mut data = vec![0u8; 16 * 1024];
        let mut count = 0;
        if unsafe {
            winapi::um::ioapiset::DeviceIoControl(
                file.as_raw_handle().cast(),
                0x900a8,
                std::ptr::null_mut(),
                0,
                data.as_mut_ptr().cast(),
                data.len() as u32,
                &mut count,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error().into());
        }
        data.truncate(count as usize);
        crate::reparse::ReparsePoint::from_bytes(
            file.metadata()?.file_attributes() & 0x10 != 0,
            data,
        )
    }

    #[cfg(windows)]
    pub(crate) fn write_reparse(
        &self,
        path: &str,
        point: &crate::reparse::ReparsePoint,
    ) -> Result<()> {
        use cap_std::fs::OpenOptionsExt;
        use std::os::windows::io::AsRawHandle;
        self.access(path, Access::Write)?;
        let (directory, name) = self.parent(path)?;
        let target = point.target()?;
        if point.tag()? == crate::reparse::SYMLINK {
            let target = target
                .strip_prefix("\\??\\")
                .map_or_else(|| target.clone(), |suffix| format!("\\\\?\\{suffix}"));
            if point.directory {
                directory.symlink_dir(target, name)?;
            } else {
                directory.symlink_file(target, name)?;
            }
        } else {
            directory.create_dir(&name)?;
            let mut options = OpenOptions::new();
            options
                .access_mode(0x4000_0000)
                .share_mode(1)
                .custom_flags(0x0220_0000)
                .follow(FollowSymlinks::No);
            let file = directory.open_with(name, &options)?;
            let mut count = 0;
            if unsafe {
                winapi::um::ioapiset::DeviceIoControl(
                    file.as_raw_handle().cast(),
                    0x900a4,
                    point.data.as_ptr().cast_mut().cast(),
                    point.data.len() as u32,
                    std::ptr::null_mut(),
                    0,
                    &mut count,
                    std::ptr::null_mut(),
                )
            } == 0
            {
                return Err(io::Error::last_os_error().into());
            }
        }
        Ok(())
    }

    /// Use a held directory capability for all actual I/O. Checking a canonical
    /// pathname earlier is not sufficient to make a later host rename safe.
    pub(crate) fn write_atomic(
        &self,
        path: &str,
        mode: u32,
        write: impl FnOnce(&mut cap_std::fs::File) -> Result<()>,
    ) -> Result<()> {
        let relative = self.access(path, Access::Write)?;
        let parent = relative.parent().unwrap_or(Path::new(""));
        (self.authorize)(&self.path.join(parent), Access::Write)?;
        let directory = self.open_directory(parent)?;
        let name = relative
            .file_name()
            .ok_or_else(|| Error::Invalid("cannot replace the root".into()))?;
        let temporary = format!(".dl-workspace-{}.tmp", uuid::Uuid::new_v4());
        (self.authorize)(&self.path.join(parent).join(&temporary), Access::Write)?;
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        let mut file = directory.open_with(&temporary, &options)?;
        let mut installed = false;
        let result = (|| -> Result<()> {
            write(&mut file)?;
            #[cfg(unix)]
            {
                use cap_std::fs::PermissionsExt;
                file.set_permissions(cap_std::fs::Permissions::from_mode(mode & 0o777))?;
            }
            #[cfg(windows)]
            {
                let mut permissions = file.metadata()?.permissions();
                permissions.set_readonly(mode & 0o222 == 0);
                file.set_permissions(permissions)?;
            }
            file.sync_all()?;
            self.access(path, Access::Write)?;
            directory.rename(&temporary, &directory, name)?;
            installed = true;
            // cap-std directory capabilities may use O_PATH on Linux. Reopen
            // the held directory itself with read access before fsync; the
            // capability's pathname is never reopened through the host tree.
            #[cfg(unix)]
            directory.open(".")?.sync_all()?;
            #[cfg(not(unix))]
            directory
                .try_clone()?
                .into_std_file()
                .sync_all()
                .or_else(|error| {
                    // Windows directory handles do not support FlushFileBuffers.
                    if cfg!(windows) && error.kind() == io::ErrorKind::PermissionDenied {
                        Ok(())
                    } else {
                        Err(error)
                    }
                })?;
            Ok(())
        })();
        if result.is_err() && !installed {
            #[cfg(windows)]
            if let Ok(metadata) = file.metadata() {
                let mut permissions = metadata.permissions();
                permissions.set_readonly(false);
                let _ = file.set_permissions(permissions);
            }
            let _ = directory.remove_file(&temporary);
        }
        result
    }

    pub(crate) fn mkdir(&self, path: &str, mode: u32) -> Result<()> {
        self.access(path, Access::Write)?;
        let (directory, name) = self.parent(path)?;
        directory.create_dir(&name)?;
        #[cfg(unix)]
        {
            use cap_std::fs::PermissionsExt;
            directory
                .open_dir_nofollow(&name)?
                .set_permissions(".", cap_std::fs::Permissions::from_mode(mode & 0o777))?;
        }
        #[cfg(not(unix))]
        let _ = mode;
        Ok(())
    }

    pub(crate) fn set_mode(&self, path: &str, mode: u32) -> Result<()> {
        self.access(path, Access::Write)?;
        let (directory, name) = self.parent(path)?;
        #[cfg(unix)]
        let permissions = {
            use cap_std::fs::PermissionsExt;
            cap_std::fs::Permissions::from_mode(mode & 0o777)
        };
        #[cfg(not(unix))]
        let permissions = {
            let mut permissions = directory.symlink_metadata(&name)?.permissions();
            permissions.set_readonly(mode & 0o222 == 0);
            permissions
        };
        #[cfg(unix)]
        {
            // Linux does not implement lchmod/fchmodat(AT_SYMLINK_NOFOLLOW).
            // Change the opened target with fchmod, retaining no-follow I/O.
            let mut options = OpenOptions::new();
            options.read(true).follow(FollowSymlinks::No);
            directory
                .open_with(name, &options)?
                .set_permissions(permissions)?;
        }
        #[cfg(not(unix))]
        directory.set_symlink_permissions(name, permissions)?;
        Ok(())
    }

    pub(crate) fn remove(&self, path: &str, directory: bool) -> Result<()> {
        self.access(path, Access::Write)?;
        let (parent, name) = self.parent(path)?;
        #[cfg(windows)]
        let directory = {
            use cap_std::fs::MetadataExt;
            directory || parent.symlink_metadata(&name)?.file_attributes() & 0x10 != 0
        };
        if directory {
            parent.remove_dir(name)?;
        } else {
            parent.remove_file_or_symlink(name)?;
        }
        Ok(())
    }

    pub(crate) fn symlink(&self, path: &str, target: &str) -> Result<()> {
        self.access(path, Access::Write)?;
        let (directory, name) = self.parent(path)?;
        #[cfg(unix)]
        directory.symlink_contents(target, name)?;
        #[cfg(windows)]
        cap_std::fs::Dir::symlink_file(&directory, target, name)?;
        #[cfg(not(any(unix, windows)))]
        return Err(Error::Unsupported("symlinks".into()));
        Ok(())
    }
}

pub(crate) fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\', '\0']) {
        return Err(Error::Invalid("invalid workspace entry name".into()));
    }
    Ok(())
}

pub(crate) fn relative_path(path: &str) -> Result<PathBuf> {
    if path.contains(['\\', '\0']) {
        return Err(Error::Invalid("workspace paths use / separators".into()));
    }
    let path = Path::new(path);
    if path
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(Error::Invalid("expected a relative workspace path".into()));
    }
    Ok(path.to_owned())
}
