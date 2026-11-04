use daat_locus_workspace::{Directory, EntryKind, Resolution, Store};
use std::{fs, path::PathBuf, sync::Arc};

fn fixture() -> (tempfile::TempDir, PathBuf, Store) {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("project");
    fs::create_dir(&path).unwrap();
    fs::write(path.join("code"), b"one\ntwo\nthree\nfour\nfive\n").unwrap();
    let path = path.canonicalize().unwrap();
    let store = Store::open(&temp.path().join("store.sqlite3")).unwrap();
    (temp, path, store)
}

fn directory(path: &std::path::Path) -> Directory {
    Directory::open(path, Arc::new(|_, _| Ok(()))).unwrap()
}

#[test]
fn text_merges_combine_disjoint_lines_and_preserve_overlapping_conflicts() {
    for (base, left, right, expected) in [
        ("a\nb\n", "A\nb\n", "a\nB\n", Some("A\nB\n")),
        ("a\nb\n", "A\nb\n", "X\nb\n", None),
        ("a\n", "x\na\n", "A\n", Some("x\nA\n")),
        ("a\n", "x\na\n", "y\na\n", None),
        ("a\nb\n", "A\nb\n", "a\nb\nx\n", Some("A\nb\nx\n")),
        ("a\nb\n", "b\n", "a\nB\n", Some("B\n")),
    ] {
        let (_temp, path, store) = fixture();
        fs::write(path.join("code"), base).unwrap();
        let first = store.branch(directory(&path), "left", "default").unwrap();
        let second = first.fork("right").unwrap();
        for (branch, text) in [(&first, left), (&second, right)] {
            branch.truncate("code", 0).unwrap();
            branch.write("code", 0, text.as_bytes()).unwrap();
        }
        let outcome = store
            .merge(&path, "left", "right", "merged", "Combine", &[])
            .unwrap();
        if let Some(expected) = expected {
            assert!(outcome.conflicts.is_empty(), "{outcome:?}");
            assert_eq!(
                store
                    .open_recorded_branch(&path, "merged")
                    .unwrap()
                    .read("code", 0, 100)
                    .unwrap(),
                expected.as_bytes()
            );
        } else {
            assert_eq!(outcome.conflicts, ["code"]);
        }
    }
}

#[test]
fn merged_versions_preserve_sources_and_ancestry_across_later_publications() {
    let (_temp, path, store) = fixture();
    let a = store.branch(directory(&path), "a", "default").unwrap();
    let initial = a.commit("Initial").unwrap();
    let b = a.fork("b").unwrap();
    a.write("code", 0, b"ONE").unwrap();
    b.write("code", 19, b"FIVE").unwrap();
    let result = store
        .merge(&path, "a", "b", "combined", "Integrate", &[])
        .unwrap();
    assert!(result.conflicts.is_empty(), "{result:?}");
    let combined = store.open_recorded_branch(&path, "combined").unwrap();
    assert_eq!(
        combined.read("code", 0, 100).unwrap(),
        b"ONE\ntwo\nthree\nfour\nFIVE\n"
    );
    assert_eq!(a.read("code", 19, 4).unwrap(), b"five");
    assert_eq!(b.read("code", 0, 3).unwrap(), b"one");
    assert_eq!(result.base, initial.id);
    let integrated = result.version.unwrap();
    assert_eq!(integrated.parents, [result.left, result.right]);

    // The next merge uses the previously integrated source, not the original import.
    a.write("code", 4, b"TWO").unwrap();
    let next = store
        .merge(&path, "combined", "a", "next", "Continue", &[])
        .unwrap();
    assert!(next.conflicts.is_empty(), "{next:?}");
    let next = store.open_recorded_branch(&path, "next").unwrap();
    fs::write(path.join("user-only"), b"native work").unwrap();
    fs::write(path.join("code"), b"one\ntwo\nthree\nFOUR\nfive\n").unwrap();
    let published = next.publish(&directory(&path)).unwrap();
    assert!(
        published.error.is_none() && published.conflicts.is_empty(),
        "{published:?}"
    );
    assert_eq!(
        fs::read(path.join("code")).unwrap(),
        b"ONE\nTWO\nthree\nFOUR\nFIVE\n"
    );
    assert_eq!(fs::read(path.join("user-only")).unwrap(), b"native work");
    assert!(next.entry("user-only").is_err());
    assert!(
        store
            .merge(&path, "a", "b", "next", "Occupied", &[])
            .is_err()
    );

    let restored = store.fork_at(&path, "earlier", &initial.id).unwrap();
    assert_eq!(
        restored.read("code", 0, 100).unwrap(),
        b"one\ntwo\nthree\nfour\nfive\n"
    );
    let native = store.commit_default(&directory(&path), "Native").unwrap();
    let rebased = store
        .merge(&path, "a", &native.id, "updated", "Update", &[])
        .unwrap();
    assert!(rebased.conflicts.is_empty(), "{rebased:?}");
    assert_eq!(
        store
            .open_recorded_branch(&path, "updated")
            .unwrap()
            .read("user-only", 0, 50)
            .unwrap(),
        b"native work"
    );
    store
        .merge(&path, "a", "b", "cross-left", "Cross merge", &[])
        .unwrap();
    store
        .merge(&path, "b", "a", "cross-right", "Cross merge", &[])
        .unwrap();
    let cross = store
        .merge(
            &path,
            "cross-left",
            "cross-right",
            "cross-result",
            "Integrate both histories",
            &[],
        )
        .unwrap();
    assert!(cross.conflicts.is_empty(), "{cross:?}");
    assert_eq!(
        store
            .open_recorded_branch(&path, "cross-result")
            .unwrap()
            .read("code", 0, 100)
            .unwrap(),
        b"ONE\nTWO\nthree\nfour\nFIVE\n"
    );
}

#[test]
fn fixed_conflict_versions_accept_custom_content_without_editing_both_inputs() {
    let (_temp, path, store) = fixture();
    fs::write(path.join("binary"), [0, 1, 2]).unwrap();
    fs::create_dir(path.join("folder")).unwrap();
    fs::write(path.join("folder/child"), b"original").unwrap();
    let left = store.branch(directory(&path), "left", "default").unwrap();
    let right = left.fork("right").unwrap();
    left.write("binary", 1, &[3]).unwrap();
    right.write("binary", 1, &[4]).unwrap();
    left.remove("folder/child", false).unwrap();
    left.remove("folder", true).unwrap();
    right.write("folder/child", 0, b"modified").unwrap();
    let conflict = store
        .merge(&path, "left", "right", "result", "Combine", &[])
        .unwrap();
    assert_eq!(conflict.conflicts, ["binary", "folder"]);
    assert!(conflict.version.is_none());
    assert!(store.open_recorded_branch(&path, "result").is_err());
    let work = store.fork_at(&path, "resolution", &conflict.right).unwrap();
    work.write("binary", 1, &[9]).unwrap();
    let resolutions = [
        Resolution {
            path: "binary".into(),
            source: Some("resolution".into()),
        },
        Resolution {
            path: "folder".into(),
            source: None,
        },
    ];
    // Source branches may continue moving; the reported versions stay stable.
    left.write("binary", 1, &[7]).unwrap();
    let resolved = store
        .merge(
            &path,
            &conflict.left,
            &conflict.right,
            "result",
            "Resolved",
            &resolutions,
        )
        .unwrap();
    assert!(resolved.conflicts.is_empty(), "{resolved:?}");
    let result = store.open_recorded_branch(&path, "result").unwrap();
    assert_eq!(result.read("binary", 0, 5).unwrap(), [0, 9, 2]);
    assert!(result.entry("folder").is_err());
    assert_eq!(right.read("binary", 0, 5).unwrap(), [0, 4, 2]);
    fs::write(path.join("binary"), [0, 8, 2]).unwrap();
    let conflict = result.publish(&directory(&path)).unwrap();
    assert_eq!(conflict.conflicts, ["binary"]);
    assert!(conflict.applied.is_empty());
    fs::write(path.join("binary"), [0, 10, 2]).unwrap();
    assert!(
        store
            .publish(
                &directory(&path),
                &conflict.source,
                Some(&conflict.target),
                &[resolutions[0].clone()]
            )
            .is_err()
    );
    assert_eq!(fs::read(path.join("binary")).unwrap(), [0, 10, 2]);
    let conflict = store
        .publish(&directory(&path), &conflict.source, None, &[])
        .unwrap();
    let published = store
        .publish(
            &directory(&path),
            &conflict.source,
            Some(&conflict.target),
            &[resolutions[0].clone()],
        )
        .unwrap();
    assert!(
        published.error.is_none() && published.conflicts.is_empty(),
        "{published:?}"
    );
    assert_eq!(fs::read(path.join("binary")).unwrap(), [0, 9, 2]);
    assert!(!path.join("folder").exists());
    assert!(
        store
            .merge(
                &path,
                &conflict.source,
                &conflict.target,
                "invalid",
                "Invalid",
                &[Resolution {
                    path: "code".into(),
                    source: None
                }]
            )
            .is_err()
    );
}

#[test]
fn history_paging_and_saved_content_survive_compaction_and_reopen() {
    let (temp, path, store) = fixture();
    assert!(
        store
            .history(&path, "default", None, 10)
            .unwrap()
            .is_empty()
    );
    let branch = store.branch(directory(&path), "work", "default").unwrap();
    let initial = branch.commit("Initial").unwrap();
    branch.write("code", 0, b"ONE").unwrap();
    let saved = branch.commit("First edit").unwrap();
    let first = store
        .history(&path, "work", None, 1)
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(first.id, saved.id);
    branch.write("code", 4, b"TWO").unwrap();
    let latest = branch.commit("Second edit").unwrap();
    assert_eq!(
        store
            .history(&path, "work", Some(first.cursor), 10)
            .unwrap()[0]
            .id,
        initial.id
    );
    let diff = store
        .diff(&path, &initial.id, &latest.id, None, 10)
        .unwrap();
    assert_eq!(
        diff.iter().map(|c| c.path.as_str()).collect::<Vec<_>>(),
        ["code"]
    );
    assert_eq!(
        store
            .read_version(&path, &saved.id, "code", 0, 7)
            .unwrap()
            .bytes,
        b"ONE\ntwo"
    );
    branch.create("unsaved", EntryKind::File, 0o644).unwrap();
    store.collect_garbage().unwrap();
    drop((branch, store));
    let store = Store::open(&temp.path().join("store.sqlite3")).unwrap();
    assert_eq!(
        store
            .read_version(&path, &initial.id, "code", 0, 3)
            .unwrap()
            .bytes,
        b"one"
    );
    assert!(
        store
            .open_recorded_branch(&path, "work")
            .unwrap()
            .entry("unsaved")
            .is_ok()
    );
    let other = temp.path().join("other");
    fs::create_dir(&other).unwrap();
    let other = other.canonicalize().unwrap();
    store.branch(directory(&other), "other", "default").unwrap();
    assert!(store.fork_at(&other, "foreign", &saved.id).is_err());
    assert!(
        store
            .read_version(&path, &saved.id, "../escape", 0, 10)
            .is_err()
    );
}

#[test]
fn upgrading_a_store_preserves_live_branches_and_legacy_checkpoints() {
    let (temp, path, store) = fixture();
    let branch = store.branch(directory(&path), "work", "default").unwrap();
    branch.write("code", 0, b"OLD").unwrap();
    let checkpoint = branch.root().unwrap();
    branch.write("code", 0, b"NEW").unwrap();
    let database = temp.path().join("store.sqlite3");
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection.execute_batch("CREATE TABLE checkpoints(seq INTEGER PRIMARY KEY,project INTEGER,name TEXT,root TEXT,reason TEXT,created_ms INTEGER);").unwrap();
    connection.execute("INSERT INTO checkpoints(project,name,root,reason,created_ms) SELECT project,name,?1,'Legacy checkpoint',1 FROM branches WHERE name='work'", [checkpoint.as_str()]).unwrap();
    connection
        .execute_batch(
            "DROP TABLE version_refs; DROP TABLE versions; UPDATE workspace_format SET version=1;",
        )
        .unwrap();
    drop((connection, branch, store));
    let store = Store::open(&database).unwrap();
    let branch = store.open_recorded_branch(&path, "work").unwrap();
    assert_eq!(branch.read("code", 0, 3).unwrap(), b"NEW");
    let history = store.history(&path, "work", None, 10).unwrap();
    let checkpoint = history
        .iter()
        .find(|v| v.message == "Legacy checkpoint")
        .unwrap();
    assert_eq!(
        store
            .fork_at(&path, "restored", &checkpoint.id)
            .unwrap()
            .read("code", 0, 3)
            .unwrap(),
        b"OLD"
    );
    assert_eq!(
        fs::read(path.join("code")).unwrap(),
        b"one\ntwo\nthree\nfour\nfive\n"
    );
}

#[test]
fn rebind_retains_versions_and_deletion_fences_recreated_branches() {
    let (temp, path, store) = fixture();
    let branch = store.branch(directory(&path), "work", "default").unwrap();
    let saved = branch.commit("Keep this version").unwrap();
    let manager = daat_locus_workspace::Workspaces::new(store.clone(), temp.path().join("mounts"));
    fs::rename(&path, temp.path().join("old-project")).unwrap();
    fs::create_dir(&path).unwrap();
    fs::write(path.join("code"), b"new native bytes").unwrap();
    assert!(
        store
            .commit_default(&directory(&path), "wrong identity")
            .is_err()
    );
    manager.rebind(&directory(&path)).unwrap();
    store
        .commit_default(&directory(&path), "Recreated directory")
        .unwrap();
    assert_eq!(
        store
            .read_version(&path, &saved.id, "code", 0, 100)
            .unwrap()
            .bytes,
        b"one\ntwo\nthree\nfour\nfive\n"
    );
    manager.delete(&path, Some("work")).unwrap();
    let recreated = store.branch(directory(&path), "work", "default").unwrap();
    assert_ne!(branch.key(), recreated.key());
    assert!(branch.write("code", 0, b"stale").is_err());
    assert!(branch.commit("stale").is_err());
    assert!(branch.fork("stale-fork").is_err());
    assert_eq!(recreated.read("code", 0, 100).unwrap(), b"new native bytes");
    assert_eq!(
        store
            .read_version(&path, &saved.id, "code", 0, 3)
            .unwrap()
            .bytes,
        b"one"
    );
    let storage = store.storage(&path).unwrap();
    assert!(storage.stored_bytes > 0);
    assert_eq!(storage.stored_bytes, storage.exclusive_stored_bytes);
    manager.delete(&path, None).unwrap();
    assert!(store.read_version(&path, &saved.id, "code", 0, 3).is_err());
    store.collect_garbage().unwrap();
    assert_eq!(store.statistics().unwrap().objects, 0);
    assert_eq!(fs::read(path.join("code")).unwrap(), b"new native bytes");
    let next = store.branch(directory(&path), "work", "default").unwrap();
    assert_ne!(next.key(), recreated.key());
    assert!(recreated.write("code", 0, b"stale project").is_err());
}

#[test]
fn project_storage_counts_shared_blocks_once_and_reclaims_only_unreferenced_content() {
    let (temp, first, store) = fixture();
    let second = temp.path().join("second");
    fs::create_dir(&second).unwrap();
    fs::copy(first.join("code"), second.join("code")).unwrap();
    store.commit_default(&directory(&first), "first").unwrap();
    store.commit_default(&directory(&second), "second").unwrap();
    let second = second.canonicalize().unwrap();
    let stats = store.storage(&first).unwrap();
    assert!(stats.shared_stored_bytes > 0);
    assert_eq!(
        stats.stored_bytes,
        stats.exclusive_stored_bytes + stats.shared_stored_bytes
    );
    let manager = daat_locus_workspace::Workspaces::new(store.clone(), temp.path().join("mounts"));
    manager.delete(&first, None).unwrap();
    store.collect_garbage().unwrap();
    let stats = store.storage(&second).unwrap();
    assert_eq!(stats.shared_stored_bytes, 0);
    assert_eq!(stats.stored_bytes, stats.exclusive_stored_bytes);
}

#[test]
fn gitignore_capture_preserves_tracked_and_new_files_and_keeps_build_outputs_isolated() {
    for (hash, split) in [("sha1", true), ("sha256", false)] {
        let (temp, path, store) = fixture();
        let git = |arguments: &[&str]| {
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(&path)
                .args(arguments)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "--quiet", &format!("--object-format={hash}")]);
        fs::create_dir(path.join("target")).unwrap();
        fs::create_dir(path.join("nested")).unwrap();
        fs::write(path.join(".gitignore"), "/target/\n*.tmp\n!keep.tmp\n").unwrap();
        fs::write(path.join("target/tracked"), b"tracked").unwrap();
        fs::write(path.join("target/cache"), b"native").unwrap();
        fs::write(path.join("new.txt"), b"new").unwrap();
        fs::write(path.join("drop.tmp"), b"ignored").unwrap();
        fs::write(path.join("keep.tmp"), b"keep").unwrap();
        fs::write(path.join("nested/.gitignore"), "*.cache\n!keep.cache\n").unwrap();
        fs::write(path.join("nested/drop.cache"), b"ignored").unwrap();
        fs::write(path.join("nested/keep.cache"), b"keep").unwrap();
        git(&["add", "-f", "target/tracked"]);
        if split {
            git(&["update-index", "--split-index"]);
        } else {
            git(&["update-index", "--index-version=4"]);
        }
        let branch = store.branch(directory(&path), "work", "default").unwrap();
        for name in ["target/cache", "drop.tmp", "nested/drop.cache"] {
            assert!(branch.entry(name).is_err(), "{name}");
        }
        for name in ["target/tracked", "new.txt", "keep.tmp", "nested/keep.cache"] {
            assert!(branch.entry(name).is_ok(), "{name}");
        }
        branch
            .create("target/generated", EntryKind::File, 0o644)
            .unwrap();
        branch
            .write("target/generated", 0, b"branch cache")
            .unwrap();
        branch.write("code", 0, b"ONE").unwrap();
        let saved = branch.commit("Source edits").unwrap();
        assert!(
            store
                .read_version(&path, &saved.id, "target/generated", 0, 100)
                .is_err()
        );
        assert_eq!(
            branch.read("target/generated", 0, 100).unwrap(),
            b"branch cache"
        );
        let published = branch.publish(&directory(&path)).unwrap();
        assert!(
            published.error.is_none() && published.conflicts.is_empty(),
            "{published:?}"
        );
        assert_eq!(fs::read(path.join("target/cache")).unwrap(), b"native");
        assert!(!path.join("target/generated").exists());
        assert!(branch.changes(0, 100).unwrap().is_empty());
        let full = store
            .branch(directory(&path).with_ignored(true), "full", "default")
            .unwrap();
        full.write("target/cache", 0, b"branch").unwrap();
        let outcome = full.publish(&directory(&path)).unwrap();
        assert!(
            outcome.error.is_none() && outcome.conflicts.is_empty(),
            "{outcome:?}"
        );
        assert_eq!(fs::read(path.join("target/cache")).unwrap(), b"branch");
        drop(temp);
    }
}

#[test]
fn committed_reads_remain_available_during_capture_and_progress_can_abort_it() {
    let (temp, path, store) = fixture();
    let branch = store
        .branch(directory(&path), "existing", "default")
        .unwrap();
    let manager = Arc::new(daat_locus_workspace::Workspaces::new(
        store.clone(),
        temp.path().join("mounts"),
    ));
    let (started, waiting) = std::sync::mpsc::channel();
    let (release, blocked) = std::sync::mpsc::channel();
    let gate = std::sync::Mutex::new(Some((started, blocked)));
    let origin = directory(&path).with_progress(move |_| {
        if let Some((started, blocked)) = gate.lock().unwrap().take() {
            started.send(()).unwrap();
            blocked.recv().unwrap();
        }
        Ok(())
    });
    let writer = store.clone();
    let capture = std::thread::spawn(move || writer.branch(origin, "new", "default"));
    waiting
        .recv_timeout(std::time::Duration::from_secs(3))
        .unwrap();
    let (sent, received) = std::sync::mpsc::channel();
    let lookup = path.clone();
    let reader = std::thread::spawn(move || {
        assert!(branch.read("code", 0, 3).is_ok());
        assert_eq!(manager.locate(&lookup).unwrap().branch, "default");
        assert!(manager.active_projections().unwrap().is_empty());
        sent.send(()).unwrap();
    });
    let responsive = received
        .recv_timeout(std::time::Duration::from_secs(2))
        .is_ok();
    release.send(()).unwrap();
    capture.join().unwrap().unwrap();
    reader.join().unwrap();
    assert!(
        responsive,
        "capture blocked committed metadata and content reads"
    );
    let origin = directory(&path).with_progress(|progress| {
        if progress.bytes > 0 {
            return Err(std::io::Error::other("capture cancelled").into());
        }
        Ok(())
    });
    assert!(store.branch(origin, "aborted", "default").is_err());
    assert!(store.open_recorded_branch(&path, "aborted").is_err());
}
