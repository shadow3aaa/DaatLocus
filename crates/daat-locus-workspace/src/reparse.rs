//! Persistent Windows link metadata. Parsing never follows the target.
use crate::{Error, Result};
use serde::{Deserialize, Serialize};

pub(crate) const SYMLINK: u32 = 0xa000000c;
pub(crate) const JUNCTION: u32 = 0xa0000003;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ReparsePoint {
    pub directory: bool,
    pub data: Vec<u8>,
}

impl ReparsePoint {
    pub fn from_bytes(directory: bool, data: Vec<u8>) -> Result<Self> {
        let point = Self { directory, data };
        point.target()?;
        Ok(point)
    }

    pub fn tag(&self) -> Result<u32> {
        let bytes = self.data.get(..4).ok_or_else(invalid)?;
        Ok(u32::from_le_bytes(bytes.try_into().map_err(|_| invalid())?))
    }

    pub fn target(&self) -> Result<String> {
        let data = &self.data;
        if data.len() < 16
            || data.len() > 16 * 1024
            || usize::from(word(data, 4)?) + 8 != data.len()
        {
            return Err(invalid());
        }
        let start = match self.tag()? {
            SYMLINK if data.len() >= 20 && dword(data, 16)? & !1 == 0 => 20,
            JUNCTION if self.directory => 16,
            SYMLINK | JUNCTION => return Err(invalid()),
            _ => return Err(Error::Unsupported("Windows reparse tag".into())),
        };
        let target = string(data, start, word(data, 8)?, word(data, 10)?)?;
        string(data, start, word(data, 12)?, word(data, 14)?)?;
        if target.is_empty() || target.contains('\0') {
            return Err(invalid());
        }
        Ok(target)
    }

    pub fn symlink(target: &str, directory: bool) -> Result<Self> {
        if target.is_empty() || target.contains('\0') {
            return Err(invalid());
        }
        let target = target.replace('/', "\\");
        let absolute = target.starts_with('\\') || target.as_bytes().get(1) == Some(&b':');
        let substitute = if let Some(suffix) = target.strip_prefix("\\\\?\\") {
            format!("\\??\\{suffix}")
        } else if target.starts_with("\\??\\") {
            target.clone()
        } else if let Some(suffix) = target.strip_prefix("\\\\") {
            format!("\\??\\UNC\\{suffix}")
        } else if absolute && target.as_bytes().get(1) == Some(&b':') {
            format!("\\??\\{target}")
        } else {
            target.clone()
        };
        let name: Vec<u8> = substitute
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        let print: Vec<u8> = target.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let length = u16::try_from(16 + name.len() + print.len()).map_err(|_| invalid())?;
        let mut data = Vec::with_capacity(usize::from(length) + 8);
        data.extend(SYMLINK.to_le_bytes());
        data.extend(length.to_le_bytes());
        data.extend(0u16.to_le_bytes());
        for value in [
            0,
            name.len() as u16,
            (name.len() + 2) as u16,
            print.len() as u16,
        ] {
            data.extend(value.to_le_bytes());
        }
        data.extend(u32::from(!absolute).to_le_bytes());
        data.extend(name);
        data.extend(0u16.to_le_bytes());
        data.extend(print);
        data.extend(0u16.to_le_bytes());
        Self::from_bytes(directory, data)
    }

    pub fn same_target(&self, other: &Self) -> bool {
        self.directory == other.directory
            && self.tag().ok() == other.tag().ok()
            && self.target().ok() == other.target().ok()
            && (self.tag().ok() != Some(SYMLINK)
                || dword(&self.data, 16).ok() == dword(&other.data, 16).ok())
    }

    /// Relocate only absolute targets within this workspace. External targets
    /// retain native semantics and never become copied project contents.
    pub fn relocate(&self, from: &std::path::Path, to: &std::path::Path) -> Result<Self> {
        use std::{
            os::windows::ffi::OsStrExt,
            path::{Component, Path, PathBuf},
        };
        let target = self.target()?;
        if self.tag()? == SYMLINK && dword(&self.data, 16)? & 1 != 0 {
            return Ok(self.clone());
        }
        let target = target
            .strip_prefix("\\??\\")
            .map_or_else(|| target.clone(), |suffix| format!("\\\\?\\{suffix}"));
        let mut normalized = PathBuf::new();
        for component in dunce::simplified(Path::new(&target)).components() {
            match component {
                Component::CurDir => {}
                Component::ParentDir => {
                    if !normalized.pop() {
                        return Err(invalid());
                    }
                }
                component => normalized.push(component.as_os_str()),
            }
        }
        let mut parts = normalized.components();
        for expected in dunce::simplified(from).components() {
            let Some(actual) = parts.next() else {
                return Ok(self.clone());
            };
            let actual: Vec<_> = actual.as_os_str().encode_wide().collect();
            let expected: Vec<_> = expected.as_os_str().encode_wide().collect();
            if unsafe {
                winapi::um::stringapiset::CompareStringOrdinal(
                    actual.as_ptr(),
                    actual.len() as i32,
                    expected.as_ptr(),
                    expected.len() as i32,
                    1,
                )
            } != 2
            {
                return Ok(self.clone());
            }
        }
        let target = dunce::simplified(to).join(parts.collect::<PathBuf>());
        let target = target.to_str().ok_or_else(invalid)?;
        if self.tag()? == SYMLINK {
            return Self::symlink(target, self.directory);
        }
        let target = Self::symlink(target, true)?.target()?;
        let name: Vec<u8> = target.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let length = u16::try_from(name.len()).map_err(|_| invalid())?;
        let data_length = u16::try_from(12 + 2 * name.len()).map_err(|_| invalid())?;
        let mut data = Vec::new();
        data.extend(JUNCTION.to_le_bytes());
        data.extend(data_length.to_le_bytes());
        data.extend(0u16.to_le_bytes());
        for value in [0, length, length + 2, length] {
            data.extend(value.to_le_bytes());
        }
        data.extend(&name);
        data.extend(0u16.to_le_bytes());
        data.extend(name);
        data.extend(0u16.to_le_bytes());
        Self::from_bytes(true, data)
    }
}

fn word(data: &[u8], offset: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(
        data.get(offset..offset + 2)
            .ok_or_else(invalid)?
            .try_into()
            .map_err(|_| invalid())?,
    ))
}
fn dword(data: &[u8], offset: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(
        data.get(offset..offset + 4)
            .ok_or_else(invalid)?
            .try_into()
            .map_err(|_| invalid())?,
    ))
}
fn string(data: &[u8], start: usize, offset: u16, length: u16) -> Result<String> {
    if !offset.is_multiple_of(2) || !length.is_multiple_of(2) {
        return Err(invalid());
    }
    let start = start + usize::from(offset);
    let bytes = data
        .get(start..start + usize::from(length))
        .ok_or_else(invalid)?;
    String::from_utf16(
        &bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect::<Vec<_>>(),
    )
    .map_err(|_| invalid())
}
fn invalid() -> Error {
    Error::Invalid("invalid Windows link reparse data".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Directory, EntryKind, Store};
    use std::{fs, path::Path, sync::Arc};

    fn junction(target: &Path) -> ReparsePoint {
        let target = format!("\\??\\{}", dunce::simplified(target).display());
        let units: Vec<_> = target.encode_utf16().collect();
        let length = u16::try_from(units.len() * 2).unwrap();
        let mut data = Vec::new();
        data.extend(JUNCTION.to_le_bytes());
        data.extend((12 + 2 * length).to_le_bytes());
        data.extend(0u16.to_le_bytes());
        for field in [0, length, length + 2, length] {
            data.extend(field.to_le_bytes());
        }
        for _ in 0..2 {
            for unit in &units {
                data.extend(unit.to_le_bytes());
            }
            data.extend(0u16.to_le_bytes());
        }
        ReparsePoint::from_bytes(true, data).unwrap()
    }

    #[test]
    fn native_junctions_snapshot_fork_reopen_and_merge_as_links() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        fs::create_dir_all(source.join("target")).unwrap();
        fs::create_dir(source.join("other")).unwrap();
        fs::write(source.join("other/file.txt"), b"other bytes").unwrap();
        fs::write(source.join("target/file.txt"), b"target bytes").unwrap();
        let origin = Directory::open(&source, Arc::new(|_, _| Ok(()))).unwrap();
        origin
            .write_reparse("original-link", &junction(&source.join("target")))
            .unwrap();
        let database = temp.path().join("store.sqlite3");
        let store = Store::open(&database).unwrap();
        let branch = store.branch(origin.clone(), "feature", "default").unwrap();
        assert_eq!(
            branch.entry("original-link").unwrap().kind,
            EntryKind::Symlink
        );
        let point = branch.reparse_point("original-link").unwrap().unwrap();
        assert_eq!(point.tag().unwrap(), JUNCTION);
        assert!(point.directory);
        assert!(branch.entry("original-link/file.txt").is_err());
        branch
            .create("new-link", EntryKind::Directory, 0o755)
            .unwrap();
        branch.set_reparse_point("new-link", Some(point)).unwrap();
        branch.remove("original-link", false).unwrap();
        branch.fork("peer").unwrap();
        drop((store, branch));
        let store = Store::open(&database).unwrap();
        let branch = store.open_branch(origin.clone(), "feature").unwrap();
        let peer = store.open_branch(origin.clone(), "peer").unwrap();
        assert!(peer.reparse_point("new-link").unwrap().unwrap().directory);
        let outcome = branch.merge_to_default(&origin).unwrap();
        assert!(
            outcome.conflicts.is_empty() && outcome.error.is_none(),
            "{outcome:?}"
        );
        assert!(!source.join("original-link").exists());
        assert_eq!(
            fs::read(source.join("new-link/file.txt")).unwrap(),
            b"target bytes"
        );
        assert_eq!(
            fs::read(source.join("target/file.txt")).unwrap(),
            b"target bytes"
        );
        branch
            .set_reparse_point("new-link", Some(junction(&source.join("other"))))
            .unwrap();
        let second = branch.merge_to_default(&origin).unwrap();
        assert!(
            second.conflicts.is_empty() && second.error.is_none(),
            "{second:?}"
        );
        assert_eq!(
            fs::read(source.join("new-link/file.txt")).unwrap(),
            b"other bytes"
        );
    }

    #[test]
    fn absolute_internal_links_relocate_without_rebinding_external_or_relative_targets() {
        let source = Path::new(r"C:\Project");
        let mount = Path::new(r"D:\Workspace\feature");
        let point = junction(Path::new(r"c:\project\packages\library"));
        let relocated = point.relocate(source, mount).unwrap();
        assert_eq!(
            relocated.target().unwrap(),
            r"\??\D:\Workspace\feature\packages\library"
        );
        assert_eq!(
            relocated.relocate(mount, source).unwrap().target().unwrap(),
            r"\??\C:\Project\packages\library"
        );
        let external = junction(Path::new(r"C:\Project-other\library"));
        assert_eq!(external.relocate(source, mount).unwrap(), external);
        let relative = ReparsePoint::symlink(r"..\packages\library", true).unwrap();
        assert_eq!(relative.relocate(source, mount).unwrap(), relative);
    }

    #[test]
    fn malformed_link_data_and_nonempty_files_do_not_publish_a_reparse_change() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("file.txt"), b"original").unwrap();
        fs::write(source.join("empty.txt"), b"").unwrap();
        let origin = Directory::open(&source, Arc::new(|_, _| Ok(()))).unwrap();
        let store = Store::open(&temp.path().join("store.sqlite3")).unwrap();
        let branch = store.branch(origin, "feature", "default").unwrap();
        let point = ReparsePoint::symlink("elsewhere", false).unwrap();
        assert!(
            branch
                .set_reparse_point("file.txt", Some(point.clone()))
                .is_err()
        );
        let mut malformed = point;
        malformed.data[8..10].copy_from_slice(&u16::MAX.to_le_bytes());
        assert!(malformed.target().is_err());
        assert!(
            branch
                .set_reparse_point("empty.txt", Some(malformed))
                .is_err()
        );
        assert_eq!(branch.read("file.txt", 0, 80).unwrap(), b"original");
        assert!(branch.changes(0, 10).unwrap().is_empty());
    }
}
