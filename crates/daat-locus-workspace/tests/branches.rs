use daat_locus_workspace::{Access, Directory, EntryKind, Store};
use std::{fs, sync::Arc};

fn directory(path: &std::path::Path) -> Directory {
    Directory::open(path, Arc::new(|_, _| Ok(()))).unwrap()
}

#[test]
fn a_source_cannot_snapshot_the_content_store_it_would_be_mutating() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    let store = Store::open(&source.join("store.sqlite3")).unwrap();
    let manager = daat_locus_workspace::Workspaces::new(store.clone(), temp.path().join("mounts"));
    let result = manager.select(directory(&source), "feature", "default");
    assert!(
        matches!(result, Err(daat_locus_workspace::Error::Invalid(_))),
        "{result:?}"
    );
    assert!(
        store
            .branches(&directory(&source), 0, 10)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn shared_identity_is_durable_and_forks_share_content_without_touching_the_source() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("work.txt"), b"original\n").unwrap();
    let database = temp.path().join("workspace.sqlite3");
    let store = Store::open(&database).unwrap();
    let first = store
        .branch(directory(&source), "feature", "default")
        .unwrap();
    let count = store.statistics().unwrap().objects;
    let peer = Store::open(&database)
        .unwrap()
        .branch(directory(&source), "feature", "default")
        .unwrap();
    let independent = first.fork("experiment").unwrap();
    assert_eq!(
        store.statistics().unwrap().objects,
        count,
        "forks reuse the immutable tree, not copied files"
    );
    first.write("work.txt", 0, b"changed!").unwrap();
    assert_eq!(peer.read("work.txt", 0, 80).unwrap(), b"changed!\n");
    assert_eq!(independent.read("work.txt", 0, 80).unwrap(), b"original\n");
    assert_eq!(fs::read(source.join("work.txt")).unwrap(), b"original\n");
    drop((first, peer, independent, store));
    let reopened = Store::open(&database)
        .unwrap()
        .branch(directory(&source), "feature", "default")
        .unwrap();
    assert_eq!(reopened.read("work.txt", 0, 80).unwrap(), b"changed!\n");
    assert_eq!(reopened.changes(0, 10).unwrap()[0].path, "work.txt");
}

#[test]
fn random_writes_holes_truncation_and_rename_preserve_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(&temp.path().join("store.sqlite3")).unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    let branch = store.branch(directory(&source), "work", "default").unwrap();
    branch.create("out", EntryKind::File, 0o644).unwrap();
    branch.write("out", 0, b"first").unwrap();
    branch.write("out", 131_074, b"last").unwrap();
    let mut expected = b"first".to_vec();
    expected.resize(131_074, 0);
    expected.extend_from_slice(b"last");
    assert_eq!(
        branch.read("out", 0, expected.len() + 50).unwrap(),
        expected
    );
    branch.write("out", 65_534, b"boundary").unwrap();
    expected[65_534..65_542].copy_from_slice(b"boundary");
    assert_eq!(
        branch.read("out", 65_530, 20).unwrap(),
        expected[65_530..65_550]
    );
    branch.truncate("out", 65_537).unwrap();
    expected.truncate(65_537);
    branch.truncate("out", 131_080).unwrap();
    expected.resize(131_080, 0);
    branch.rename("out", "renamed", false).unwrap();
    assert!(branch.entry("out").is_err());
    assert_eq!(branch.read("renamed", 0, usize::MAX).unwrap(), expected);
    assert!(!source.join("renamed").exists());
}

#[test]
fn merge_preserves_unrelated_user_changes_and_merges_disjoint_text_edits() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("code.txt"), b"one\ntwo\nthree\nfour\nfive\n").unwrap();
    fs::write(source.join("user.txt"), b"before").unwrap();
    let store = Store::open(&temp.path().join("store.sqlite3")).unwrap();
    let branch = store
        .branch(directory(&source), "feature", "default")
        .unwrap();
    branch.write("code.txt", 0, b"ONE").unwrap();
    fs::write(source.join("code.txt"), b"one\ntwo\nthree\nfour\nFIVE\n").unwrap();
    fs::write(source.join("user.txt"), b"user work").unwrap();
    assert_eq!(
        branch.read("code.txt", 0, 80).unwrap(),
        b"ONE\ntwo\nthree\nfour\nfive\n",
        "host changes must not leak into the pinned base"
    );
    let outcome = branch.publish(&directory(&source)).unwrap();
    assert!(outcome.conflicts.is_empty(), "{outcome:?}");
    assert!(outcome.error.is_none(), "{outcome:?}");
    assert_eq!(
        fs::read(source.join("code.txt")).unwrap(),
        b"ONE\ntwo\nthree\nfour\nFIVE\n"
    );
    assert_eq!(fs::read(source.join("user.txt")).unwrap(), b"user work");
    assert!(branch.changes(0, 20).unwrap().is_empty());
    assert_eq!(
        branch.read("code.txt", 0, 80).unwrap(),
        b"ONE\ntwo\nthree\nfour\nfive\n",
        "publishing must not replace the mounted branch with the native directory"
    );
    assert_eq!(branch.read("user.txt", 0, 80).unwrap(), b"before");
    branch.write("code.txt", 4, b"TWO").unwrap();
    let next = branch.publish(&directory(&source)).unwrap();
    assert!(
        next.error.is_none() && next.conflicts.is_empty(),
        "{next:?}"
    );
    assert_eq!(
        fs::read(source.join("code.txt")).unwrap(),
        b"ONE\nTWO\nthree\nfour\nFIVE\n",
        "later publishes preserve the native edits missing from the isolated branch"
    );
}

#[test]
fn conflict_is_detected_before_any_source_write() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("a.txt"), b"before").unwrap();
    fs::write(source.join("binary"), [0, 1, 2]).unwrap();
    let store = Store::open(&temp.path().join("store.sqlite3")).unwrap();
    let branch = store
        .branch(directory(&source), "feature", "default")
        .unwrap();
    branch.write("a.txt", 0, b"after!").unwrap();
    branch.write("binary", 1, &[3]).unwrap();
    fs::write(source.join("binary"), [0, 4, 2]).unwrap();
    let outcome = branch.publish(&directory(&source)).unwrap();
    assert_eq!(outcome.conflicts, ["binary"]);
    assert!(outcome.applied.is_empty());
    assert_eq!(fs::read(source.join("a.txt")).unwrap(), b"before");
    assert_eq!(fs::read(source.join("binary")).unwrap(), [0, 4, 2]);
}

#[test]
fn interrupted_merge_retains_branch_and_is_not_replayed_when_reopened() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    let database = temp.path().join("store.sqlite3");
    let store = Store::open(&database).unwrap();
    let branch = store
        .branch(directory(&source), "feature", "default")
        .unwrap();
    for name in ["a.txt", "b.txt"] {
        branch.create(name, EntryKind::File, 0o644).unwrap();
        branch.write(name, 0, name.as_bytes()).unwrap();
    }
    let attempts = std::sync::atomic::AtomicUsize::new(0);
    let target = Directory::open(
        &source,
        Arc::new(move |path, access| {
            if access == Access::Write
                && path.file_name().is_some_and(|name| name == "b.txt")
                && attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst) > 0
            {
                return Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied).into());
            }
            Ok(())
        }),
    )
    .unwrap();
    let outcome = branch.publish(&target).unwrap();
    assert_eq!(outcome.applied, ["a.txt"]);
    assert_eq!(outcome.uncertain.as_deref(), Some("b.txt"));
    assert!(outcome.error.is_some());
    drop((branch, store));
    let reopened = Store::open(&database)
        .unwrap()
        .branch(directory(&source), "feature", "default")
        .unwrap();
    assert!(
        !source.join("b.txt").exists(),
        "reopen must not replay a started merge"
    );
    assert_eq!(reopened.read("b.txt", 0, 80).unwrap(), b"b.txt");
    let outcome = reopened.publish(&directory(&source)).unwrap();
    assert_eq!(
        outcome.applied,
        ["b.txt"],
        "a new explicit merge only applies missing work"
    );
    assert_eq!(fs::read(source.join("b.txt")).unwrap(), b"b.txt");
}

#[test]
fn replaced_source_directory_cannot_receive_old_branch_effects() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("file"), b"old").unwrap();
    let store = Store::open(&temp.path().join("store.sqlite3")).unwrap();
    let branch = store
        .branch(directory(&source), "feature", "default")
        .unwrap();
    branch.write("file", 0, b"new").unwrap();
    fs::rename(&source, temp.path().join("moved")).unwrap();
    fs::create_dir(&source).unwrap();
    fs::write(source.join("file"), b"replacement").unwrap();
    assert!(branch.publish(&directory(&source)).is_err());
    assert_eq!(
        branch.read("file", 0, 80).unwrap(),
        b"new",
        "a snapshot must not retain a directory lock on the native source"
    );
    let reopened = store
        .open_recorded_branch(branch.origin(), "feature")
        .unwrap();
    assert_eq!(reopened.read("file", 0, 80).unwrap(), b"new");
    assert_eq!(
        reopened
            .fork("after-move")
            .unwrap()
            .read("file", 0, 80)
            .unwrap(),
        b"new"
    );
    assert!(
        store
            .branch(directory(&source), "feature", "default")
            .is_err()
    );
    assert_eq!(fs::read(source.join("file")).unwrap(), b"replacement");
}

#[test]
fn moved_source_paths_keep_independent_branch_identities() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("file"), b"original").unwrap();
    let store_path = temp.path().join("store.sqlite3");
    let store = Store::open(&store_path).unwrap();
    let original = store
        .branch(directory(&source), "feature", "default")
        .unwrap();
    original.write("file", 0, b"old edit").unwrap();

    let moved = temp.path().join("moved");
    fs::rename(&source, &moved).unwrap();
    let moved_directory = directory(&moved);
    assert!(original.publish(&moved_directory).is_err());
    let relocated = store
        .branch(moved_directory.clone(), "feature", "default")
        .unwrap();
    assert_ne!(original.key(), relocated.key());
    assert_eq!(relocated.read("file", 0, 80).unwrap(), b"original");
    relocated.write("file", 0, b"new edit").unwrap();
    assert_eq!(original.read("file", 0, 80).unwrap(), b"old edit");
    let outcome = relocated.publish(&moved_directory).unwrap();
    assert!(outcome.conflicts.is_empty() && outcome.error.is_none());
    assert_eq!(fs::read(moved.join("file")).unwrap(), b"new edit");

    let reopened = Store::open(&store_path).unwrap();
    assert_eq!(
        reopened
            .open_recorded_branch(original.origin(), "feature")
            .unwrap()
            .read("file", 0, 80)
            .unwrap(),
        b"old edit"
    );
    assert_eq!(
        reopened
            .open_recorded_branch(relocated.origin(), "feature")
            .unwrap()
            .read("file", 0, 80)
            .unwrap(),
        b"new edit"
    );
}

#[test]
fn every_source_descendant_is_authorized_and_a_failed_import_publishes_no_branch() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("public"), b"allowed").unwrap();
    fs::write(source.join("private"), b"not allowed").unwrap();
    let denied = Directory::open(
        &source,
        Arc::new(|path, _| {
            if path.file_name().is_some_and(|name| name == "private") {
                return Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied).into());
            }
            Ok(())
        }),
    )
    .unwrap();
    let store = Store::open(&temp.path().join("store.sqlite3")).unwrap();
    assert!(store.branch(denied, "feature", "default").is_err());
    assert!(
        store
            .branches(&directory(&source), 0, 20)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store.statistics().unwrap().objects,
        0,
        "a refused import rolls back its private content too"
    );
}

#[test]
fn replacing_a_directory_with_a_file_removes_only_the_known_branch_children() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir_all(source.join("folder")).unwrap();
    fs::write(source.join("folder/child"), b"old").unwrap();
    let store = Store::open(&temp.path().join("store.sqlite3")).unwrap();
    let branch = store
        .branch(directory(&source), "feature", "default")
        .unwrap();
    branch.remove("folder/child", false).unwrap();
    branch.remove("folder", true).unwrap();
    branch.create("folder", EntryKind::File, 0o644).unwrap();
    branch.write("folder", 0, b"replacement").unwrap();
    fs::write(source.join("folder/user-added"), b"user work").unwrap();
    let conflict = branch.publish(&directory(&source)).unwrap();
    assert_eq!(conflict.conflicts, ["folder"]);
    assert_eq!(
        fs::read(source.join("folder/user-added")).unwrap(),
        b"user work"
    );
    assert_eq!(fs::read(source.join("folder/child")).unwrap(), b"old");
    fs::remove_file(source.join("folder/user-added")).unwrap();
    let completed = branch.publish(&directory(&source)).unwrap();
    assert!(completed.error.is_none(), "{completed:?}");
    assert_eq!(fs::read(source.join("folder")).unwrap(), b"replacement");
}

#[test]
fn changed_file_blocks_are_shared_across_large_branch_copies() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    let bytes: Vec<_> = (0..524_288_u32)
        .map(|i| ((i.wrapping_mul(2654435761) >> 24) & 255) as u8)
        .collect();
    fs::write(source.join("data"), &bytes).unwrap();
    let store = Store::open(&temp.path().join("store.sqlite3")).unwrap();
    let first = store.branch(directory(&source), "a", "default").unwrap();
    let second = first.fork("b").unwrap();
    let before = store.statistics().unwrap().uncompressed_bytes;
    second.write("data", 131_076, b"edit").unwrap();
    let growth = store.statistics().unwrap().uncompressed_bytes - before;
    assert!(
        growth < 140_000,
        "a four-byte edit must not duplicate the whole 512 KiB file: {growth}"
    );
    assert_eq!(
        first.read("data", 131_076, 4).unwrap(),
        bytes[131_076..131_080]
    );
    assert_eq!(second.read("data", 131_076, 4).unwrap(), b"edit");
}

#[test]
fn damaged_objects_are_refused_instead_of_returned_as_file_content() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("data"), b"original").unwrap();
    let database = temp.path().join("store.sqlite3");
    let store = Store::open(&database).unwrap();
    let branch = store.branch(directory(&source), "a", "default").unwrap();
    let connection = rusqlite::Connection::open(&database).unwrap();
    let wrong = zstd::bulk::compress(b"tampered", 3).unwrap();
    connection
        .execute("UPDATE objects SET data=?1 WHERE domain=0", [wrong])
        .unwrap();
    assert!(matches!(
        branch.read("data", 0, 80),
        Err(daat_locus_workspace::Error::Corrupt(_))
    ));
}

#[test]
fn collection_reclaims_old_versions_and_sqlite_pages_without_deleting_shared_branches() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("file"), b"original").unwrap();
    let database = temp.path().join("store.sqlite3");
    let store = Store::open(&database).unwrap();
    let first = store
        .branch(directory(&source), "first", "default")
        .unwrap();
    let peer = first.fork("peer").unwrap();
    let mut bytes: Vec<_> = (0..65_536_u32)
        .map(|i| (i.wrapping_mul(2654435761) >> 24) as u8)
        .collect();
    for _ in 0..12 {
        for byte in &mut bytes {
            *byte = byte.wrapping_add(1);
        }
        first.write("file", 0, &bytes).unwrap();
    }
    let before = store.statistics().unwrap().stored_bytes;
    let disk_before = fs::metadata(&database).unwrap().len()
        + fs::metadata(database.with_file_name("store.sqlite3-wal"))
            .unwrap()
            .len();
    let result = store.collect_garbage().unwrap();
    assert!(result.removed_objects > 0 && result.removed_bytes > 0);
    assert!(store.statistics().unwrap().stored_bytes < before);
    assert!(
        fs::metadata(&database).unwrap().len() < disk_before,
        "collection must reclaim SQLite pages on disk"
    );
    assert_eq!(first.read("file", 0, usize::MAX).unwrap(), bytes);
    assert_eq!(peer.read("file", 0, 80).unwrap(), b"original");
    assert_eq!(fs::read(source.join("file")).unwrap(), b"original");
    drop((first, peer, store));
    let reopened = Store::open(&database)
        .unwrap()
        .open_branch(directory(&source), "first")
        .unwrap();
    assert_eq!(reopened.read("file", 0, usize::MAX).unwrap(), bytes);
}

#[cfg(unix)]
#[test]
fn directory_permission_changes_are_published_without_changing_its_children() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir_all(source.join("folder")).unwrap();
    fs::write(source.join("folder/child"), b"user bytes").unwrap();
    fs::set_permissions(source.join("folder"), fs::Permissions::from_mode(0o755)).unwrap();
    let store = Store::open(&temp.path().join("store.sqlite3")).unwrap();
    let branch = store
        .branch(directory(&source), "feature", "default")
        .unwrap();
    branch.set_metadata("folder", Some(0o750), None).unwrap();
    let outcome = branch.publish(&directory(&source)).unwrap();
    assert!(
        outcome.error.is_none() && outcome.conflicts.is_empty(),
        "{outcome:?}"
    );
    assert_eq!(
        fs::metadata(source.join("folder"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o750
    );
    assert_eq!(
        fs::read(source.join("folder/child")).unwrap(),
        b"user bytes"
    );
}

#[cfg(windows)]
#[test]
#[allow(clippy::permissions_set_readonly_false)] // Only clears a Windows temporary fixture's readonly attribute.
fn newly_published_readonly_files_keep_their_attribute() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    let store = Store::open(&temp.path().join("store.sqlite3")).unwrap();
    let branch = store
        .branch(directory(&source), "feature", "default")
        .unwrap();
    branch.create("readonly", EntryKind::File, 0o644).unwrap();
    branch.write("readonly", 0, b"readonly bytes").unwrap();
    branch.set_metadata("readonly", Some(0o444), None).unwrap();
    let outcome = branch.publish(&directory(&source)).unwrap();
    assert!(
        outcome.error.is_none() && outcome.conflicts.is_empty(),
        "{outcome:?}"
    );
    assert_eq!(
        fs::read(source.join("readonly")).unwrap(),
        b"readonly bytes"
    );
    assert!(
        fs::metadata(source.join("readonly"))
            .unwrap()
            .permissions()
            .readonly()
    );
    let mut permissions = fs::metadata(source.join("readonly")).unwrap().permissions();
    permissions.set_readonly(false);
    fs::set_permissions(source.join("readonly"), permissions).unwrap();
}

#[cfg(unix)]
#[test]
fn symlinks_are_captured_as_links_without_importing_external_contents() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(temp.path().join("outside"), b"outside secret").unwrap();
    std::os::unix::fs::symlink("../outside", source.join("link")).unwrap();
    let store = Store::open(&temp.path().join("store.sqlite3")).unwrap();
    let branch = store.branch(directory(&source), "a", "default").unwrap();
    assert_eq!(branch.read_link("link").unwrap(), "../outside");
    assert!(branch.read("link", 0, 80).is_err());
    assert!(branch.read("../outside", 0, 80).is_err());
    assert!(branch.create("../escape", EntryKind::File, 0o644).is_err());
}
