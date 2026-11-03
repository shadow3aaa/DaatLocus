//! Opt in on a machine with its native filesystem dependency installed.
#![cfg(any(windows, target_os = "linux"))]
use daat_locus_workspace::{Directory, Store, Workspaces, native};
use std::{fs, sync::Arc};

#[test]
#[ignore = "requires the installed WinFsp driver or a working FUSE device and mount helper"]
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
#[ignore = "requires WinFsp or a working FUSE device and mount helper"]
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
#[ignore = "requires the installed WinFsp driver and permission to mount directories"]
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
#[ignore = "requires the installed WinFsp driver and permission to mount directories"]
fn windows_native_programs_can_execute_from_a_branch() {
    use std::os::windows::process::CommandExt;
    let (_temp, _source, manager, location) = windows_fixture();
    let executable = location.directory.join("command.exe");
    let system = std::path::PathBuf::from(std::env::var_os("WINDIR").unwrap());
    fs::copy(system.join("System32/cmd.exe"), &executable).unwrap();
    let mut child = std::process::Command::new(executable)
        .args(["/D", "/C", "exit 37"])
        .current_dir(&location.directory)
        .creation_flags(0x08000000)
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("native branch executable did not exit");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    assert_eq!(status.code(), Some(37));
    manager.close();
}

#[cfg(windows)]
#[test]
#[ignore = "requires the installed WinFsp driver and permission to mount directories"]
fn windows_symbolic_links_follow_branch_bytes_and_survive_fork_and_restore() {
    use std::os::windows::fs::{symlink_dir, symlink_file};
    let (temp, source, manager, location) = windows_fixture();
    fs::create_dir(location.directory.join("packages")).unwrap();
    fs::write(location.directory.join("packages/value.txt"), b"nested\n").unwrap();
    symlink_file("file.txt", location.directory.join("file-link")).unwrap();
    symlink_dir("packages", location.directory.join("directory-link")).unwrap();
    assert_eq!(
        fs::read_link(location.directory.join("file-link")).unwrap(),
        std::path::Path::new("file.txt")
    );
    let fork = manager
        .select_recorded(&location.path, "fork", "windows")
        .unwrap()
        .unwrap();
    fs::write(location.directory.join("file-link"), b"linked edit\n").unwrap();
    fs::write(
        location.directory.join("directory-link/value.txt"),
        b"nested edit\n",
    )
    .unwrap();
    assert_eq!(
        fs::read(location.directory.join("file.txt")).unwrap(),
        b"linked edit\n"
    );
    assert_eq!(
        fs::read(fork.directory.join("file-link")).unwrap(),
        b"original\n"
    );
    assert_eq!(
        fs::read(fork.directory.join("directory-link/value.txt")).unwrap(),
        b"nested\n"
    );
    assert_eq!(fs::read(source.join("file.txt")).unwrap(), b"original\n");
    fs::remove_dir(location.directory.join("directory-link")).unwrap();
    assert_eq!(
        fs::read(location.directory.join("packages/value.txt")).unwrap(),
        b"nested edit\n"
    );
    symlink_dir("packages", location.directory.join("directory-link")).unwrap();
    manager.close();
    drop(manager);
    let manager = Workspaces::new(
        Store::open(&temp.path().join("store.sqlite3")).unwrap(),
        temp.path().join("mounts"),
    );
    let restored = manager.restore(&location.directory).unwrap();
    assert_eq!(
        fs::read(restored.directory.join("file-link")).unwrap(),
        b"linked edit\n"
    );
    assert_eq!(
        fs::read(restored.directory.join("directory-link/value.txt")).unwrap(),
        b"nested edit\n"
    );
    manager.close();
}

#[cfg(windows)]
fn junction(link: &std::path::Path, target: &std::path::Path) {
    use std::{os::windows::fs::OpenOptionsExt, os::windows::io::AsRawHandle};
    fs::create_dir(link).unwrap();
    let file = fs::OpenOptions::new()
        .access_mode(0x40000000)
        .custom_flags(0x02200000)
        .open(link)
        .unwrap();
    let target = format!("\\??\\{}", dunce::simplified(target).display()).replace('/', "\\");
    let target: Vec<u8> = target.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let length = u16::try_from(target.len()).unwrap();
    let mut data = Vec::new();
    data.extend(0xa0000003u32.to_le_bytes());
    data.extend((12 + 2 * length).to_le_bytes());
    data.extend(0u16.to_le_bytes());
    for field in [0, length, length + 2, length] {
        data.extend(field.to_le_bytes());
    }
    data.extend(&target);
    data.extend(0u16.to_le_bytes());
    data.extend(target);
    data.extend(0u16.to_le_bytes());
    let mut count = 0;
    assert_ne!(
        unsafe {
            winapi::um::ioapiset::DeviceIoControl(
                file.as_raw_handle().cast(),
                0x900a4,
                data.as_mut_ptr().cast(),
                data.len() as u32,
                std::ptr::null_mut(),
                0,
                &mut count,
                std::ptr::null_mut(),
            )
        },
        0,
        "{}",
        std::io::Error::last_os_error()
    );
}

#[cfg(windows)]
#[test]
#[ignore = "requires the installed WinFsp driver and permission to mount directories"]
fn windows_internal_absolute_junctions_relocate_and_merge_without_touching_source_targets() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir_all(source.join("packages/library")).unwrap();
    fs::write(source.join("packages/library/value.txt"), b"original\n").unwrap();
    junction(&source.join("dependency"), &source.join("packages/library"));
    let origin = Directory::open(&source, Arc::new(|_, _| Ok(()))).unwrap();
    let store_path = temp.path().join("store.sqlite3");
    let mount_root = temp.path().join("mounts");
    let manager = Workspaces::new(Store::open(&store_path).unwrap(), mount_root.clone());
    let location = manager
        .select(origin.clone(), "feature", "default")
        .unwrap();
    assert_eq!(
        fs::read_link(location.directory.join("dependency"))
            .unwrap()
            .canonicalize()
            .unwrap(),
        location
            .directory
            .join("packages/library")
            .canonicalize()
            .unwrap()
    );
    fs::write(location.directory.join("dependency/value.txt"), b"branch\n").unwrap();
    assert_eq!(
        fs::read(source.join("dependency/value.txt")).unwrap(),
        b"original\n"
    );
    assert_eq!(
        fs::read(location.directory.join("packages/library/value.txt")).unwrap(),
        b"branch\n"
    );
    junction(
        &location.directory.join("new-dependency"),
        &location.directory.join("packages/library"),
    );
    let fork = manager
        .select_recorded(&location.path, "fork", "feature")
        .unwrap()
        .unwrap();
    assert_eq!(
        fs::read(fork.directory.join("new-dependency/value.txt")).unwrap(),
        b"branch\n"
    );
    fs::write(
        location.directory.join("packages/library/value.txt"),
        b"updated\n",
    )
    .unwrap();
    assert_eq!(
        fs::read(fork.directory.join("dependency/value.txt")).unwrap(),
        b"branch\n"
    );
    manager.close();
    drop(manager);
    let manager = Workspaces::new(Store::open(&store_path).unwrap(), mount_root);
    manager.restore(&location.directory).unwrap();
    assert_eq!(
        fs::read(location.directory.join("new-dependency/value.txt")).unwrap(),
        b"updated\n"
    );
    let branch = manager
        .store()
        .open_branch(origin.clone(), "feature")
        .unwrap();
    let outcome = branch.merge_to_default(&origin).unwrap();
    assert!(
        outcome.conflicts.is_empty() && outcome.error.is_none(),
        "{outcome:?}"
    );
    assert_eq!(
        fs::read(source.join("new-dependency/value.txt")).unwrap(),
        b"updated\n"
    );
    manager.close();
    // The published junction must point into default, independently of mounts.
    assert_eq!(
        fs::read(source.join("new-dependency/value.txt")).unwrap(),
        b"updated\n"
    );
}

#[cfg(windows)]
#[test]
#[ignore = "requires WinFsp and native NTFS symlink creation privilege or Developer Mode"]
fn windows_native_symbolic_links_snapshot_and_merge_with_their_directory_type() {
    use std::os::windows::fs::{symlink_dir, symlink_file};
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir_all(source.join("packages")).unwrap();
    fs::write(source.join("packages/value.txt"), b"original\n").unwrap();
    symlink_file(r"packages\value.txt", source.join("source-link"))
        .expect("native NTFS symlink creation privilege or Developer Mode is required");
    symlink_dir("packages", source.join("source-directory-link")).unwrap();
    assert_eq!(fs::read(source.join("source-link")).unwrap(), b"original\n");
    let origin = Directory::open(&source, Arc::new(|_, _| Ok(()))).unwrap();
    let manager = Workspaces::new(
        Store::open(&temp.path().join("store.sqlite3")).unwrap(),
        temp.path().join("mounts"),
    );
    let location = manager
        .select(origin.clone(), "feature", "default")
        .unwrap();
    assert_eq!(
        fs::read(location.directory.join("source-link")).unwrap(),
        b"original\n"
    );
    assert_eq!(
        fs::read(location.directory.join("source-directory-link/value.txt")).unwrap(),
        b"original\n"
    );
    symlink_file(
        r"packages\value.txt",
        location.directory.join("published-file-link"),
    )
    .unwrap();
    symlink_dir(
        "packages",
        location.directory.join("published-directory-link"),
    )
    .unwrap();
    fs::write(location.directory.join("source-link"), b"branch\n").unwrap();
    let branch = manager
        .store()
        .open_branch(origin.clone(), "feature")
        .unwrap();
    let result = branch.merge_to_default(&origin).unwrap();
    assert!(
        result.conflicts.is_empty() && result.error.is_none(),
        "{result:?}"
    );
    manager.close();
    assert_eq!(
        fs::read(source.join("published-file-link")).unwrap(),
        b"branch\n"
    );
    assert_eq!(
        fs::read(source.join("published-directory-link/value.txt")).unwrap(),
        b"branch\n"
    );
}

#[cfg(windows)]
#[test]
#[ignore = "requires the installed WinFsp driver and permission to mount directories"]
fn windows_unsupported_hard_links_and_streams_preserve_branch_and_source_files() {
    let (_temp, source, manager, location) = windows_fixture();
    let path = location.directory.join("file.txt");
    assert!(fs::hard_link(&path, location.directory.join("hard-link")).is_err());
    assert!(!location.directory.join("hard-link").exists());
    assert!(fs::write(location.directory.join("file.txt:extra"), b"stream").is_err());
    assert_eq!(fs::read(&path).unwrap(), b"original\n");
    assert_eq!(fs::read(source.join("file.txt")).unwrap(), b"original\n");
    manager.close();
}

#[cfg(windows)]
#[test]
#[ignore = "requires WinFsp"]
fn windows_recursive_deletion_handles_empty_and_nested_directories() {
    let (_temp, _source, manager, location) = windows_fixture();
    fs::create_dir(location.directory.join("empty")).unwrap();
    fs::remove_dir_all(location.directory.join("empty")).unwrap();
    fs::create_dir_all(location.directory.join("nested/child")).unwrap();
    fs::write(location.directory.join("nested/child/file.txt"), b"bytes").unwrap();
    fs::remove_dir_all(location.directory.join("nested")).unwrap();
    assert!(!location.directory.join("empty").exists());
    assert!(!location.directory.join("nested").exists());
    manager.close();
}

#[cfg(windows)]
fn run_development_command(
    mut command: std::process::Command,
    logs: &std::path::Path,
) -> (std::process::ExitStatus, String) {
    use std::os::windows::process::CommandExt;
    let log = tempfile::NamedTempFile::new_in(logs).unwrap();
    command
        .stdout(log.as_file().try_clone().unwrap())
        .stderr(log.as_file().try_clone().unwrap())
        .env_remove("NODE_OPTIONS")
        .env_remove("NODE_PATH")
        .creation_flags(0x08000000);
    let mut child = command.spawn().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!(
                "development command timed out: {}",
                fs::read_to_string(log.path()).unwrap()
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    (status, fs::read_to_string(log.path()).unwrap())
}

#[cfg(windows)]
#[test]
#[ignore = "requires WinFsp, Node.js and pnpm on PATH"]
fn windows_pnpm_installs_and_resolves_local_packages_in_shared_and_isolated_branches() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir_all(source.join("packages/helper")).unwrap();
    fs::write(
        source.join("package.json"),
        r#"{"private":true,"dependencies":{"local-helper":"file:packages/helper"}}"#,
    )
    .unwrap();
    fs::write(
        source.join("packages/helper/package.json"),
        r#"{"name":"local-helper","version":"1.0.0","main":"index.js"}"#,
    )
    .unwrap();
    fs::write(
        source.join("packages/helper/index.js"),
        b"module.exports = 7;\n",
    )
    .unwrap();
    let config = temp.path().join("empty.npmrc");
    fs::write(&config, "").unwrap();
    let global_config = temp.path().join("empty-global.npmrc");
    fs::write(&global_config, "").unwrap();
    let manager = Workspaces::new(
        Store::open(&temp.path().join("store.sqlite3")).unwrap(),
        temp.path().join("mounts"),
    );
    let origin = Directory::open(&source, Arc::new(|_, _| Ok(()))).unwrap();
    let location = manager.select(origin, "feature", "default").unwrap();
    fs::write(
        location.directory.join("packages/helper/index.js"),
        b"module.exports = 42;\n",
    )
    .unwrap();
    let mut install = std::process::Command::new("pnpm.cmd");
    install
        .current_dir(&location.directory)
        .args([
            "install",
            "--offline",
            "--ignore-scripts",
            "--lockfile=false",
            "--config.manage-package-manager-versions=false",
        ])
        .arg("--store-dir")
        .arg(temp.path().join("pnpm-store"))
        .env("NPM_CONFIG_USERCONFIG", &config)
        .env("NPM_CONFIG_GLOBALCONFIG", &global_config);
    let (status, log) = run_development_command(install, temp.path());
    assert!(status.success(), "{status}: {log}");
    let fork = manager
        .select_recorded(&location.path, "peer", "feature")
        .unwrap()
        .unwrap();
    let evaluate = |directory: &std::path::Path| {
        let mut command = std::process::Command::new("node.exe");
        command
            .current_dir(directory)
            .args(["-e", "process.exit(require('local-helper'))"]);
        run_development_command(command, temp.path())
    };
    let (status, log) = evaluate(&location.directory);
    assert_eq!(status.code(), Some(42), "{log}");
    fs::write(
        location
            .directory
            .join("node_modules/local-helper/index.js"),
        b"module.exports = 55;\n",
    )
    .unwrap();
    let (status, log) = evaluate(&location.directory);
    assert_eq!(status.code(), Some(55), "{log}");
    let (status, log) = evaluate(&fork.directory);
    assert_eq!(status.code(), Some(42), "{log}");
    assert!(!source.join("node_modules").exists());
    assert_eq!(
        fs::read(source.join("packages/helper/index.js")).unwrap(),
        b"module.exports = 7;\n"
    );
    manager.close();
}

#[cfg(windows)]
#[test]
#[ignore = "requires WinFsp and the Rust toolchain"]
fn windows_cargo_builds_and_runs_a_project_with_an_internal_junction_dependency() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir_all(source.join("src")).unwrap();
    fs::create_dir_all(source.join("packages/helper/src")).unwrap();
    fs::write(source.join("Cargo.toml"), "[package]\nname='workspace-fixture'\nversion='0.1.0'\nedition='2024'\n[dependencies]\nhelper={path='dependency'}\n").unwrap();
    fs::write(
        source.join("src/main.rs"),
        "fn main() { std::process::exit(helper::value()); }\n",
    )
    .unwrap();
    fs::write(
        source.join("packages/helper/Cargo.toml"),
        "[package]\nname='helper'\nversion='0.1.0'\nedition='2024'\n",
    )
    .unwrap();
    fs::write(
        source.join("packages/helper/src/lib.rs"),
        "pub fn value() -> i32 { 7 }\n",
    )
    .unwrap();
    junction(&source.join("dependency"), &source.join("packages/helper"));
    let manager = Workspaces::new(
        Store::open(&temp.path().join("store.sqlite3")).unwrap(),
        temp.path().join("mounts"),
    );
    let origin = Directory::open(&source, Arc::new(|_, _| Ok(()))).unwrap();
    let location = manager.select(origin, "feature", "default").unwrap();
    fs::write(
        location.directory.join("dependency/src/lib.rs"),
        "pub fn value() -> i32 { 42 }\n",
    )
    .unwrap();
    let mut build = std::process::Command::new(env!("CARGO"));
    build
        .current_dir(&location.directory)
        .args(["build", "--offline", "--quiet"])
        .arg("--target-dir")
        .arg(location.directory.join("target"));
    let (status, log) = run_development_command(build, temp.path());
    assert!(status.success(), "{status}: {log}");
    let mut run = std::process::Command::new(
        location
            .directory
            .join("target/debug/workspace-fixture.exe"),
    );
    run.current_dir(&location.directory);
    let (status, log) = run_development_command(run, temp.path());
    assert_eq!(status.code(), Some(42), "{log}");
    assert!(!source.join("target").exists());
    assert_eq!(
        fs::read_to_string(source.join("packages/helper/src/lib.rs")).unwrap(),
        "pub fn value() -> i32 { 7 }\n"
    );
    manager.close();
}
