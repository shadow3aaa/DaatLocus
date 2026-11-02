//! Opt in on a machine with its native filesystem dependency installed.
#![cfg(any(windows, target_os = "linux"))]
use daat_locus_workspace::{Directory, Store, Workspaces, native};
use std::{fs, sync::Arc};

#[test]
#[ignore = "requires the installed Dokan 2 driver or a working FUSE device and mount helper"]
fn native_shared_branches_isolate_host_files_and_restore_after_unmount() {
    assert!(
        native::available().unwrap(),
        "native filesystem dependency is unavailable"
    );
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("source.txt"), b"original\n").unwrap();
    let origin = Directory::open(&source, Arc::new(|_, _| Ok(()))).unwrap();
    let store_path = temp.path().join("store.sqlite3");
    let mount_root = temp.path().join("mounts");
    let manager = Workspaces::new(Store::open(&store_path).unwrap(), mount_root.clone());
    let first = manager.select(origin.clone(), "shared", "default").unwrap();
    assert_eq!(
        manager
            .locate(&first.directory.canonicalize().unwrap())
            .unwrap()
            .branch,
        "shared",
        "canonical mount-root spelling must preserve workspace identity"
    );
    #[cfg(windows)]
    {
        assert!(
            native::open_mount_directory(&first.directory)
                .unwrap()
                .is_some()
        );
        assert!(native::open_mount_directory(&source).unwrap().is_none());
    }
    let peer = manager.select(origin.clone(), "shared", "default").unwrap();
    let other = manager
        .select_recorded(first.path.as_path(), "other", "shared")
        .unwrap()
        .expect("forking a named branch must not snapshot the native source");
    assert!(
        manager
            .select_recorded(first.path.as_path(), "fresh-default", "default")
            .unwrap()
            .is_none(),
        "an absent branch from default needs a new source snapshot"
    );
    fs::write(first.directory.join("source.txt"), b"shared edit\n").unwrap();
    assert_eq!(
        fs::read(peer.directory.join("source.txt")).unwrap(),
        b"shared edit\n"
    );
    assert_eq!(
        fs::read(other.directory.join("source.txt")).unwrap(),
        b"original\n"
    );
    assert_eq!(fs::read(source.join("source.txt")).unwrap(), b"original\n");
    fs::create_dir(first.directory.join("nested")).unwrap();
    fs::write(first.directory.join("nested/new.txt"), b"new bytes").unwrap();
    fs::rename(
        first.directory.join("nested/new.txt"),
        first.directory.join("nested/renamed.txt"),
    )
    .unwrap();
    assert_eq!(
        fs::read(peer.directory.join("nested/renamed.txt")).unwrap(),
        b"new bytes"
    );
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        let exclusive = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(first.directory.join("source.txt"))
            .unwrap();
        assert_eq!(
            fs::File::open(peer.directory.join("source.txt"))
                .unwrap_err()
                .raw_os_error(),
            Some(32)
        );
        drop(exclusive);
        assert_eq!(
            fs::read(peer.directory.join("source.txt")).unwrap(),
            b"shared edit\n"
        );
    }
    manager.close();
    #[cfg(windows)]
    assert!(
        native::open_mount_directory(&first.directory)
            .unwrap()
            .is_none()
    );
    drop(manager);
    let manager = Workspaces::new(Store::open(&store_path).unwrap(), mount_root);
    let restored = manager.restore(&first.directory).unwrap();
    assert_eq!(restored.branch, "shared");
    assert_eq!(
        fs::read(restored.directory.join("nested/renamed.txt")).unwrap(),
        b"new bytes"
    );
    let branch = manager
        .store()
        .open_branch(origin.clone(), "shared")
        .unwrap();
    let outcome = branch.merge_to_default(&origin).unwrap();
    assert!(
        outcome.conflicts.is_empty() && outcome.error.is_none(),
        "{outcome:?}"
    );
    assert_eq!(
        fs::read(source.join("source.txt")).unwrap(),
        b"shared edit\n"
    );
    assert_eq!(
        fs::read(source.join("nested/renamed.txt")).unwrap(),
        b"new bytes"
    );
    manager.close();
}

#[test]
#[ignore = "requires the installed Dokan 2 driver or a working FUSE device and mount helper"]
fn runtime_mount_probe_uses_the_native_backend() {
    let temp = tempfile::tempdir().unwrap();
    assert!(
        native::probe(temp.path()).unwrap(),
        "native filesystem backend is unavailable"
    );
}

#[cfg(windows)]
fn windows_fixture() -> (
    tempfile::TempDir,
    std::path::PathBuf,
    Workspaces,
    daat_locus_workspace::Location,
) {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("file.txt"), b"original\n").unwrap();
    let manager = Workspaces::new(
        Store::open(&temp.path().join("store.sqlite3")).unwrap(),
        temp.path().join("mounts"),
    );
    let origin = Directory::open(&source, Arc::new(|_, _| Ok(()))).unwrap();
    let location = manager.select(origin, "windows", "default").unwrap();
    (temp, source, manager, location)
}

#[cfg(windows)]
#[test]
#[ignore = "requires the installed Dokan 2 driver and permission to mount directories"]
fn windows_mapped_writes_reach_branch_storage_and_range_locks_remain_enforced() {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use winapi::um::{
        fileapi::{LockFileEx, UnlockFileEx},
        memoryapi::{
            CreateFileMappingW, FILE_MAP_WRITE, FlushViewOfFile, MapViewOfFile, UnmapViewOfFile,
        },
        minwinbase::OVERLAPPED,
        winnt::PAGE_READWRITE,
    };
    let (_temp, source, manager, location) = windows_fixture();
    let path = location.directory.join("file.txt");
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    unsafe {
        let mapping = CreateFileMappingW(
            file.as_raw_handle().cast(),
            std::ptr::null_mut(),
            PAGE_READWRITE,
            0,
            0,
            std::ptr::null(),
        );
        assert!(!mapping.is_null(), "{}", std::io::Error::last_os_error());
        let _mapping = OwnedHandle::from_raw_handle(mapping.cast());
        let view = MapViewOfFile(mapping, FILE_MAP_WRITE, 0, 0, 9);
        assert!(!view.is_null(), "{}", std::io::Error::last_os_error());
        struct MappingView(*mut std::ffi::c_void);
        impl Drop for MappingView {
            fn drop(&mut self) {
                unsafe {
                    UnmapViewOfFile(self.0);
                }
            }
        }
        let _view = MappingView(view);
        std::ptr::copy_nonoverlapping(b"mapped!!".as_ptr(), view.cast::<u8>(), 8);
        assert_ne!(
            FlushViewOfFile(view, 9),
            0,
            "{}",
            std::io::Error::last_os_error()
        );
    }
    file.sync_all().unwrap();
    assert_eq!(
        manager
            .store()
            .open_recorded_branch(&location.path, "windows")
            .unwrap()
            .read("file.txt", 0, 80)
            .unwrap(),
        b"mapped!!\n"
    );
    assert_eq!(fs::read(source.join("file.txt")).unwrap(), b"original\n");
    let peer = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    unsafe {
        let mut first: OVERLAPPED = std::mem::zeroed();
        let mut second: OVERLAPPED = std::mem::zeroed();
        const EXCLUSIVE_NOW: u32 = 0x1 | 0x2;
        assert_ne!(
            LockFileEx(
                file.as_raw_handle().cast(),
                EXCLUSIVE_NOW,
                0,
                1,
                0,
                &mut first
            ),
            0
        );
        assert_eq!(
            LockFileEx(
                peer.as_raw_handle().cast(),
                EXCLUSIVE_NOW,
                0,
                1,
                0,
                &mut second
            ),
            0
        );
        assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(33));
        assert_ne!(
            UnlockFileEx(file.as_raw_handle().cast(), 0, 1, 0, &mut first),
            0
        );
        assert_ne!(
            LockFileEx(
                peer.as_raw_handle().cast(),
                EXCLUSIVE_NOW,
                0,
                1,
                0,
                &mut second
            ),
            0
        );
        assert_ne!(
            UnlockFileEx(peer.as_raw_handle().cast(), 0, 1, 0, &mut second),
            0
        );
    }
    drop((file, peer));
    manager.close();
}

#[cfg(windows)]
#[test]
#[ignore = "requires the installed Dokan 2 driver and permission to mount directories"]
fn windows_unsupported_links_and_streams_do_not_modify_branch_or_source_files() {
    use std::os::windows::io::AsRawHandle;
    use winapi::um::ioapiset::DeviceIoControl;
    let (_temp, source, manager, location) = windows_fixture();
    let path = location.directory.join("file.txt");
    assert!(fs::hard_link(&path, location.directory.join("hard-link")).is_err());
    assert!(!location.directory.join("hard-link").exists());
    assert!(fs::write(location.directory.join("file.txt:extra"), b"stream").is_err());

    let link = location.directory.join("link");
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&link)
        .unwrap();
    let target: Vec<u16> = "file.txt".encode_utf16().collect();
    let length = (target.len() * 2) as u16;
    let mut buffer = Vec::new();
    buffer.extend(0xa000000cu32.to_le_bytes()); // IO_REPARSE_TAG_SYMLINK
    buffer.extend((12 + length).to_le_bytes());
    buffer.extend(0u16.to_le_bytes());
    for value in [0, length, 0, length] {
        buffer.extend(value.to_le_bytes());
    }
    buffer.extend(1u32.to_le_bytes()); // relative symbolic link
    for unit in target {
        buffer.extend(unit.to_le_bytes());
    }
    let mut returned = 0;
    let result = unsafe {
        DeviceIoControl(
            file.as_raw_handle().cast(),
            0x900a4,
            buffer.as_mut_ptr().cast(),
            buffer.len() as u32,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    assert_eq!(result, 0);
    assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(1));
    drop(file);
    assert_eq!(fs::read(&link).unwrap(), b"");
    assert_eq!(fs::read(&path).unwrap(), b"original\n");
    assert_eq!(fs::read(source.join("file.txt")).unwrap(), b"original\n");
    manager.close();
}
