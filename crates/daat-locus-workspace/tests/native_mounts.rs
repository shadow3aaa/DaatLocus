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
