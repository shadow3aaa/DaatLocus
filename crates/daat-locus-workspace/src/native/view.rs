use crate::{Entry, EntryKind, Error, Result, Workspace, missing, workspace::DetachedFile};
use parking_lot::Mutex;
use std::{collections::HashMap, io};

pub(super) const READ: u8 = 1;
pub(super) const WRITE: u8 = 2;
pub(super) const DELETE: u8 = 4;
pub(super) const SHARE_ALL: u8 = READ | WRITE | DELETE;

struct Open {
    inode: u64,
    access: u8,
    share: u8,
    append: bool,
}

struct State {
    paths: HashMap<u64, String>,
    handles: HashMap<u64, Open>,
    unlinked: HashMap<u64, DetachedFile>,
    next_handle: u64,
}

/// Serializes directory changes with open-handle bookkeeping. Content remains
/// in the shared durable engine; this table only holds native inode lifetimes.
pub(super) struct View {
    workspace: Workspace,
    state: Mutex<State>,
    #[cfg(target_os = "linux")]
    root_inode: u64,
}

impl View {
    #[cfg(target_os = "linux")]
    pub(super) fn inode_path(&self, inode: u64) -> Result<String> {
        Ok(Self::path(&self.state.lock(), inode)?.to_owned())
    }
    pub(super) fn backing_path(&self) -> &std::path::Path {
        self.workspace.backing_path()
    }
    #[cfg(windows)]
    pub(super) fn handle_inode(&self, handle: u64) -> Result<u64> {
        self.state
            .lock()
            .handles
            .get(&handle)
            .map(|open| open.inode)
            .ok_or_else(|| missing("file handle"))
    }
    #[cfg(windows)]
    pub(super) fn handle_path(&self, handle: u64) -> Result<String> {
        let state = self.state.lock();
        let inode = state
            .handles
            .get(&handle)
            .ok_or_else(|| missing("file handle"))?
            .inode;
        Ok(Self::path(&state, inode)?.to_owned())
    }
    pub(super) fn new(workspace: Workspace) -> Result<Self> {
        let root_inode = workspace.entry("")?.inode;
        Ok(Self {
            workspace,
            #[cfg(target_os = "linux")]
            root_inode,
            state: Mutex::new(State {
                paths: HashMap::from([(root_inode, String::new())]),
                handles: HashMap::new(),
                unlinked: HashMap::new(),
                next_handle: 0,
            }),
        })
    }

    #[cfg(target_os = "linux")]
    pub(super) fn root_inode(&self) -> u64 {
        self.root_inode
    }

    fn remember(state: &mut State, path: &str, entry: &Entry) {
        state.paths.insert(entry.inode, path.into());
    }

    fn path(state: &State, inode: u64) -> Result<&str> {
        state
            .paths
            .get(&inode)
            .map(String::as_str)
            .ok_or_else(|| missing("unlinked inode"))
    }

    pub(super) fn lookup(&self, path: &str) -> Result<Entry> {
        let mut state = self.state.lock();
        let entry = self.workspace.entry(path)?;
        Self::remember(&mut state, path, &entry);
        Ok(entry)
    }

    #[cfg(target_os = "linux")]
    pub(super) fn child(&self, parent: u64, name: &str) -> Result<String> {
        crate::directory::validate_name(name)?;
        let state = self.state.lock();
        let parent = Self::path(&state, parent)?;
        Ok(if parent.is_empty() {
            name.into()
        } else {
            format!("{parent}/{name}")
        })
    }

    pub(super) fn entry(&self, inode: u64) -> Result<Entry> {
        let state = self.state.lock();
        if let Some(file) = state.unlinked.get(&inode) {
            return Ok(file.entry());
        }
        self.workspace.entry(Self::path(&state, inode)?)
    }

    pub(super) fn open(&self, path: &str, access: u8, share: u8, append: bool) -> Result<u64> {
        let mut state = self.state.lock();
        let entry = self.workspace.entry(path)?;
        if access & WRITE != 0 && entry.mode & 0o222 == 0 {
            return Err(io::Error::from(io::ErrorKind::PermissionDenied).into());
        }
        if state.handles.values().any(|open| {
            open.inode == entry.inode && (open.access & !share != 0 || access & !open.share != 0)
        }) {
            return Err(io::Error::new(io::ErrorKind::WouldBlock, "file sharing conflict").into());
        }
        Self::remember(&mut state, path, &entry);
        state.next_handle = state
            .next_handle
            .checked_add(1)
            .ok_or_else(|| Error::Invalid("native handle counter overflow".into()))?;
        let handle = state.next_handle;
        state.handles.insert(
            handle,
            Open {
                inode: entry.inode,
                access,
                share,
                append,
            },
        );
        Ok(handle)
    }

    pub(super) fn close(&self, handle: u64) {
        let mut state = self.state.lock();
        if let Some(open) = state.handles.remove(&handle)
            && !state
                .handles
                .values()
                .any(|other| other.inode == open.inode)
        {
            state.unlinked.remove(&open.inode);
        }
    }

    pub(super) fn read(&self, handle: u64, offset: u64, length: usize) -> Result<Vec<u8>> {
        let state = self.state.lock();
        let open = state
            .handles
            .get(&handle)
            .ok_or_else(|| missing("file handle"))?;
        if open.access & READ == 0 {
            return Err(io::Error::from(io::ErrorKind::PermissionDenied).into());
        }
        if let Some(file) = state.unlinked.get(&open.inode) {
            return file.read(offset, length);
        }
        self.workspace
            .read(Self::path(&state, open.inode)?, offset, length)
    }

    pub(super) fn write(
        &self,
        handle: u64,
        offset: u64,
        bytes: &[u8],
        append: bool,
    ) -> Result<usize> {
        self.write_inner(handle, offset, bytes, append, false)
    }

    #[cfg(windows)]
    pub(super) fn write_paging(&self, handle: u64, offset: u64, bytes: &[u8]) -> Result<usize> {
        self.write_inner(handle, offset, bytes, false, true)
    }

    fn write_inner(
        &self,
        handle: u64,
        mut offset: u64,
        mut bytes: &[u8],
        append: bool,
        paging: bool,
    ) -> Result<usize> {
        let mut state = self.state.lock();
        let open = state
            .handles
            .get(&handle)
            .ok_or_else(|| missing("file handle"))?;
        if open.access & WRITE == 0 {
            return Err(io::Error::from(io::ErrorKind::PermissionDenied).into());
        }
        let inode = open.inode;
        let append = !paging && (append || open.append);
        if let Some(file) = state.unlinked.get_mut(&inode) {
            if append {
                offset = file.entry().size;
            }
            if paging {
                let length = file
                    .entry()
                    .size
                    .saturating_sub(offset)
                    .min(bytes.len() as u64);
                bytes = &bytes[..length as usize];
            }
            return if bytes.is_empty() {
                Ok(0)
            } else {
                file.write(offset, bytes)
            };
        }
        let path = Self::path(&state, inode)?;
        if append {
            offset = self.workspace.entry(path)?.size;
        }
        if paging {
            // Windows flushes complete mapped pages, including padding beyond
            // EOF. Paging I/O must never grow the file or apply append mode.
            let length = self
                .workspace
                .entry(path)?
                .size
                .saturating_sub(offset)
                .min(bytes.len() as u64);
            bytes = &bytes[..length as usize];
        }
        if bytes.is_empty() {
            Ok(0)
        } else {
            self.workspace.write(path, offset, bytes)
        }
    }

    pub(super) fn truncate(&self, inode: u64, size: u64) -> Result<()> {
        let mut state = self.state.lock();
        if let Some(file) = state.unlinked.get_mut(&inode) {
            return file.truncate(size);
        }
        self.workspace.truncate(Self::path(&state, inode)?, size)
    }

    pub(super) fn create(&self, path: &str, directory: bool, mode: u32) -> Result<Entry> {
        let mut state = self.state.lock();
        let entry = self.workspace.create(
            path,
            if directory {
                EntryKind::Directory
            } else {
                EntryKind::File
            },
            mode,
        )?;
        Self::remember(&mut state, path, &entry);
        Ok(entry)
    }

    #[cfg(windows)]
    pub(super) fn can_remove(
        &self,
        path: &str,
        directory: bool,
        acting: Option<u64>,
    ) -> Result<Entry> {
        let state = self.state.lock();
        self.can_remove_locked(&state, path, directory, acting)
    }

    fn can_remove_locked(
        &self,
        state: &State,
        path: &str,
        directory: bool,
        acting: Option<u64>,
    ) -> Result<Entry> {
        let entry = self.workspace.entry(path)?;
        if (entry.kind == EntryKind::Directory) != directory {
            return Err(Error::Invalid("entry type mismatch".into()));
        }
        if directory && !self.workspace.read_dir(path)?.is_empty() {
            return Err(io::Error::from(io::ErrorKind::DirectoryNotEmpty).into());
        }
        Self::delete_sharing(state, entry.inode, acting)?;
        Ok(entry)
    }

    fn delete_sharing(state: &State, inode: u64, acting: Option<u64>) -> Result<()> {
        if let Some(handle) = acting {
            let open = state
                .handles
                .get(&handle)
                .ok_or_else(|| missing("file handle"))?;
            if open.inode != inode || open.access & DELETE == 0 {
                return Err(io::Error::from(io::ErrorKind::PermissionDenied).into());
            }
        }
        if state.handles.iter().any(|(handle, open)| {
            Some(*handle) != acting && open.inode == inode && open.share & DELETE == 0
        }) {
            return Err(io::Error::new(io::ErrorKind::WouldBlock, "file sharing conflict").into());
        }
        Ok(())
    }

    pub(super) fn remove(&self, path: &str, directory: bool, acting: Option<u64>) -> Result<()> {
        let mut state = self.state.lock();
        let entry = self.can_remove_locked(&state, path, directory, acting)?;
        let detached = if state.handles.values().any(|open| open.inode == entry.inode) {
            Some(self.workspace.detached_file(path)?)
        } else {
            None
        };
        self.workspace.remove(path, directory)?;
        state.paths.remove(&entry.inode);
        if let Some(detached) = detached {
            state.unlinked.insert(entry.inode, detached);
        }
        Ok(())
    }

    pub(super) fn rename(
        &self,
        from: &str,
        to: &str,
        replace: bool,
        acting: Option<u64>,
    ) -> Result<()> {
        if from == to {
            return Ok(());
        }
        let mut state = self.state.lock();
        let source = self.workspace.entry(from)?;
        Self::delete_sharing(&state, source.inode, acting)?;
        let target = match self.workspace.entry(to) {
            Ok(entry) => Some(entry),
            Err(Error::Io(error)) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        let detached = if let Some(target) = &target {
            self.can_remove_locked(&state, to, target.kind == EntryKind::Directory, None)?;
            if state
                .handles
                .values()
                .any(|open| open.inode == target.inode)
            {
                Some(self.workspace.detached_file(to)?)
            } else {
                None
            }
        } else {
            None
        };
        self.workspace.rename(from, to, replace)?;
        if let Some(target) = target {
            state.paths.remove(&target.inode);
            if let Some(detached) = detached {
                state.unlinked.insert(target.inode, detached);
            }
        }
        for path in state.paths.values_mut() {
            if path == from {
                *path = to.into();
            } else if let Some(suffix) = path.strip_prefix(&format!("{from}/")) {
                *path = format!("{to}/{suffix}");
            }
        }
        Ok(())
    }

    pub(super) fn read_dir(&self, inode: u64) -> Result<Vec<(String, Entry)>> {
        let mut state = self.state.lock();
        let parent = Self::path(&state, inode)?.to_owned();
        let entries = self.workspace.read_dir(&parent)?;
        for (name, entry) in &entries {
            let path = if parent.is_empty() {
                name.clone()
            } else {
                format!("{parent}/{name}")
            };
            Self::remember(&mut state, &path, entry);
        }
        Ok(entries)
    }

    pub(super) fn set_metadata(
        &self,
        inode: u64,
        mode: Option<u32>,
        modified_ns: Option<i64>,
    ) -> Result<()> {
        let state = self.state.lock();
        self.workspace
            .set_metadata(Self::path(&state, inode)?, mode, modified_ns)
    }

    #[cfg(target_os = "linux")]
    pub(super) fn read_link(&self, inode: u64) -> Result<String> {
        let state = self.state.lock();
        self.workspace.read_link(Self::path(&state, inode)?)
    }

    #[cfg(windows)]
    pub(super) fn reparse_point(&self, inode: u64) -> Result<Option<crate::reparse::ReparsePoint>> {
        let state = self.state.lock();
        if let Some(file) = state.unlinked.get(&inode) {
            return Ok(file.reparse_point());
        }
        self.workspace.reparse_point(Self::path(&state, inode)?)
    }

    #[cfg(windows)]
    pub(super) fn set_reparse_point(
        &self,
        inode: u64,
        point: Option<crate::reparse::ReparsePoint>,
    ) -> Result<()> {
        let state = self.state.lock();
        self.workspace
            .set_reparse_point(Self::path(&state, inode)?, point)
    }

    #[cfg(target_os = "linux")]
    pub(super) fn symlink(&self, path: &str, target: &str) -> Result<Entry> {
        let mut state = self.state.lock();
        let entry = self.workspace.symlink(path, target)?;
        Self::remember(&mut state, path, &entry);
        Ok(entry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Directory, Store};
    use std::sync::Arc;

    fn fixture() -> (tempfile::TempDir, View, Store) {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir(&source).unwrap();
        let store = Store::open(&temp.path().join("store.sqlite3")).unwrap();
        let source = Directory::open(&source, Arc::new(|_, _| Ok(()))).unwrap();
        let view = View::new(store.branch(source, "a", "default").unwrap()).unwrap();
        (temp, view, store)
    }

    #[test]
    fn open_handles_follow_renames_and_survive_unlink_without_resurrecting_files() {
        let (_temp, view, _store) = fixture();
        view.create("first", false, 0o644).unwrap();
        let first = view.open("first", READ | WRITE, SHARE_ALL, false).unwrap();
        view.write(first, 0, b"hello", false).unwrap();
        view.rename("first", "second", false, None).unwrap();
        assert_eq!(view.read(first, 0, 20).unwrap(), b"hello");
        let second = view.open("second", READ | WRITE, SHARE_ALL, false).unwrap();
        view.remove("second", false, None).unwrap();
        view.write(first, 5, b" world", false).unwrap();
        assert_eq!(view.read(second, 0, 30).unwrap(), b"hello world");
        assert!(view.lookup("second").is_err());
        view.close(first);
        view.close(second);
        assert!(view.state.lock().unlinked.is_empty());
    }

    #[test]
    fn windows_sharing_rules_reject_conflicting_opens_and_release_on_close() {
        let (_temp, view, _store) = fixture();
        view.create("file", false, 0o644).unwrap();
        let first = view.open("file", READ, READ, false).unwrap();
        assert!(view.open("file", WRITE, SHARE_ALL, false).is_err());
        assert!(view.remove("file", false, None).is_err());
        assert!(view.rename("file", "renamed", false, None).is_err());
        view.close(first);
        let second = view.open("file", READ | WRITE, SHARE_ALL, false).unwrap();
        view.remove("file", false, None).unwrap();
        view.write(second, 0, b"detached", false).unwrap();
        assert_eq!(view.read(second, 0, 20).unwrap(), b"detached");
        view.close(second);
    }

    #[test]
    fn collection_keeps_unlinked_file_bytes_until_the_last_native_handle_closes() {
        let (_temp, view, store) = fixture();
        view.create("file", false, 0o644).unwrap();
        let handle = view.open("file", READ | WRITE, SHARE_ALL, false).unwrap();
        view.write(handle, 0, b"held bytes", false).unwrap();
        view.remove("file", false, None).unwrap();
        store.collect_garbage().unwrap();
        assert_eq!(view.read(handle, 0, 80).unwrap(), b"held bytes");
        view.write(handle, 10, b" after collection", false).unwrap();
        store.collect_garbage().unwrap();
        assert_eq!(
            view.read(handle, 0, 80).unwrap(),
            b"held bytes after collection"
        );
        let pinned_bytes = store.statistics().unwrap().uncompressed_bytes;
        view.close(handle);
        let collected = store.collect_garbage().unwrap();
        assert!(collected.removed_bytes > 0);
        assert!(store.statistics().unwrap().uncompressed_bytes < pinned_bytes);
        assert!(view.lookup("file").is_err());
    }

    #[test]
    fn an_exclusive_delete_handle_can_rename_and_unlink_its_own_file() {
        let (_temp, view, _store) = fixture();
        view.create("file", false, 0o644).unwrap();
        let handle = view.open("file", READ | WRITE | DELETE, 0, false).unwrap();
        view.write(handle, 0, b"exclusive bytes", false).unwrap();
        view.rename("file", "renamed", false, Some(handle)).unwrap();
        view.remove("renamed", false, Some(handle)).unwrap();
        assert!(view.lookup("renamed").is_err());
        assert_eq!(view.read(handle, 0, 80).unwrap(), b"exclusive bytes");
        view.close(handle);
    }
}
