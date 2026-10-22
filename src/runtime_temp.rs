//! Profile-local, application-owned temporary resources.
//!
//! Only this runtime writes here. Actor policies must deny writes to `root()`;
//! narrowly authorized built-in resource reads are a separate Files concern.
//! Cleanup trusts an OS lock, never a PID or the age of a directory.
use crate::file_io::guarded_directory::GuardedDirectory;
use miette::{IntoDiagnostic, Result, miette};
use std::{
    ffi::{OsStr, OsString},
    fs::File,
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
};

const INSTANCE_PREFIX: &str = "instance-";
const LEASE: &str = ".lease";
const LIFECYCLE: &str = ".lifecycle.lock";
const MARKER: &[u8] = b"DaatLocus runtime temporary resources v1\n";
const MAX_CLEANUP_ENTRIES: usize = 16_384;
const MAX_CLEANUP_DEPTH: usize = 64;

/// One runtime instance, not a daemon or a process-wide resource registry.
/// Keep this value alive until every consumer has stopped using its resources.
pub(crate) struct RuntimeTemp {
    root_path: PathBuf,
    path: PathBuf,
    root: GuardedDirectory,
    instance: Option<GuardedDirectory>,
    lease: Option<File>,
    coordination: File,
}

struct LifecycleLock<'a>(&'a File);
impl<'a> LifecycleLock<'a> {
    fn acquire(file: &'a File) -> io::Result<Self> {
        file.lock()?;
        Ok(Self(file))
    }
}
impl Drop for LifecycleLock<'_> {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

impl RuntimeTemp {
    /// The caller supplies this profile's absolute, normalized runtime directory.
    /// Symlink/reparse ancestors are rejected rather than canonicalized through.
    pub(crate) fn create(runtime_dir: &Path) -> Result<Self> {
        let runtime = GuardedDirectory::open(runtime_dir, true).into_diagnostic()?;
        runtime.protect_private().into_diagnostic()?;
        let root = runtime.child(OsStr::new("temp"), true).into_diagnostic()?;
        root.protect_private().into_diagnostic()?;
        let root_path = runtime_dir.join("temp");
        let coordination = match root.create_private_file(OsStr::new(LIFECYCLE)) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                root.open_regular(OsStr::new(LIFECYCLE), true)
                    .into_diagnostic()?
            }
            Err(error) => return Err(error).into_diagnostic(),
        };
        root.protect_private_file(OsStr::new(LIFECYCLE), &coordination)
            .into_diagnostic()?;
        let (path, instance, lease) = {
            let _lifecycle = LifecycleLock::acquire(&coordination).into_diagnostic()?;
            sweep(&root);
            let name = format!("{INSTANCE_PREFIX}{}", uuid::Uuid::new_v4().simple());
            // Exclusive mkdir never adopts a pre-existing directory, even in
            // the extraordinarily unlikely event of a UUID collision.
            let instance = root.create_child(OsStr::new(&name)).into_diagnostic()?;
            instance.protect_private().into_diagnostic()?;
            let mut lease = instance
                .create_private_file(OsStr::new(LEASE))
                .into_diagnostic()?;
            instance
                .protect_private_file(OsStr::new(LEASE), &lease)
                .into_diagnostic()?;
            lease
                .try_lock()
                .map_err(|error| miette!("unable to lock temporary instance: {error}"))?;
            lease.write_all(MARKER).into_diagnostic()?;
            lease.sync_all().into_diagnostic()?;
            instance.sync().into_diagnostic()?;
            root.sync().into_diagnostic()?;
            (root_path.join(name), instance, lease)
        };
        Ok(Self {
            root_path,
            path,
            root,
            instance: Some(instance),
            lease: Some(lease),
            coordination,
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Deny Actor writes to the whole managed namespace, including other live
    /// instances and lock files. Do not turn this root into an ambient read grant.
    pub(crate) fn root(&self) -> &Path {
        &self.root_path
    }

    /// Publish one immutable file atomically beneath skills/ or resources/.
    /// Existing destinations are never replaced. Hot-loading uses a new runtime
    /// instance, so readers cannot observe a half-written skill.
    pub(crate) fn publish(&self, relative: &Path, bytes: &[u8]) -> Result<PathBuf> {
        let components = relative.components().collect::<Vec<_>>();
        if components.len() < 2
            || !matches!(
                components[0],
                Component::Normal(name) if name == "skills" || name == "resources"
            )
            || components.iter().any(|component| {
                !matches!(component, Component::Normal(name) if resource_name(name))
            })
        {
            return Err(miette!(
                "temporary resource needs a normal skills/ or resources/ path"
            ));
        }
        let instance = self
            .instance
            .as_ref()
            .ok_or_else(|| miette!("temporary instance is closed"))?;
        let mut directories = Vec::new();
        for component in &components[..components.len() - 1] {
            let Component::Normal(name) = component else {
                unreachable!()
            };
            let parent = directories.last().unwrap_or(instance);
            let directory = parent.child(name, true).into_diagnostic()?;
            directory.protect_private().into_diagnostic()?;
            directories.push(directory);
        }
        let directory = directories.last().expect("resource has a parent");
        let target = relative.file_name().expect("normal resource filename");
        let staging = OsString::from(format!(".dl-write-{}", uuid::Uuid::new_v4().simple()));
        let mut staging_created = false;
        let result = (|| -> io::Result<()> {
            let mut file = directory.create_private_file(&staging)?;
            staging_created = true;
            directory.protect_private_file(&staging, &file)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            // Windows publication cannot unlink a source with this handle open.
            drop(file);
            directory.publish_new(&staging, target)?;
            directory.sync()
        })();
        if result.is_err() && staging_created {
            let _ = directory.remove(&staging);
        }
        result.into_diagnostic()?;
        Ok(self.path().join(relative))
    }
}

impl Drop for RuntimeTemp {
    fn drop(&mut self) {
        let result = (|| -> io::Result<()> {
            let _lifecycle = LifecycleLock::acquire(&self.coordination)?;
            let Some(instance) = self.instance.take() else {
                return Ok(());
            };
            let Some(lease) = self.lease.take() else {
                return Ok(());
            };
            let name = self.path.file_name().expect("instance has a filename");
            remove_instance(&self.root, name, instance, lease)
        })();
        if let Err(error) = result {
            // Drop cannot report failure to its caller. The next startup retries
            // a recognized orphan; unknown or altered trees remain untouched.
            tracing::warn!(%error, "temporary resource cleanup was deferred");
        }
    }
}

// Embedded resource names are portable, not arbitrary platform paths. Exclude
// Windows streams, device names and trailing-dot aliases even on Unix builds.
fn resource_name(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    if name.is_empty()
        || name.starts_with('.')
        || name.ends_with('.')
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
    {
        return false;
    }
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    !matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        && !(stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'))
}

fn instance_name(name: &OsStr) -> bool {
    let Some(name) = name
        .to_str()
        .and_then(|name| name.strip_prefix(INSTANCE_PREFIX))
    else {
        return false;
    };
    name.len() == 32
        && name
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        && uuid::Uuid::parse_str(name).is_ok()
}

/// Called only while holding the shared lifecycle lock. The stable lock file is
/// deliberately never unlinked, otherwise two processes could lock two inodes.
fn sweep(root: &GuardedDirectory) {
    let names = match root.names() {
        Ok(names) => names,
        Err(error) => {
            tracing::warn!(%error, "temporary resource sweep was deferred");
            return;
        }
    };
    for name in names.into_iter().filter(|name| instance_name(name)) {
        let result = (|| -> io::Result<()> {
            let directory = root.child(&name, false)?;
            let entries = directory.names()?;
            if entries.is_empty() {
                // Interrupted before creating .lease, or after deleting it.
                // No recursively traversed content exists in this case.
                drop(directory);
                return root.remove_empty_child(&name);
            }
            let mut lease = directory.open_regular(OsStr::new(LEASE), true)?;
            if lease.try_lock().is_err() {
                return Ok(()); // Live instance, unsupported lock, or unknown I/O: preserve it.
            }
            let mut marker = Vec::new();
            (&mut lease)
                .take(MARKER.len() as u64 + 1)
                .read_to_end(&mut marker)?;
            let incomplete = entries.len() == 1
                && entries[0] == OsStr::new(LEASE)
                && MARKER.starts_with(&marker);
            if marker != MARKER && !incomplete {
                return Ok(()); // Unknown version or contents are not ours to remove.
            }
            remove_instance(root, &name, directory, lease)
        })();
        if let Err(error) = result {
            tracing::debug!(%error, "unrecognized or unavailable temporary entry was preserved");
        }
    }
}

/// Preflight the whole owned tree before mutation. Keep every directory pinned,
/// and refuse links, special files, cross-device trees and excessive depth/size.
struct Cleanup {
    directory: GuardedDirectory,
    files: Vec<OsString>,
    children: Vec<(OsString, Cleanup)>,
}
impl Cleanup {
    fn prepare(
        directory: GuardedDirectory,
        keep_lease: bool,
        depth: usize,
        remaining: &mut usize,
    ) -> io::Result<Self> {
        if depth > MAX_CLEANUP_DEPTH {
            return Err(io::Error::other("temporary tree is too deep to clean safely"));
        }
        let mut files = Vec::new();
        let mut children = Vec::new();
        for name in directory.names()? {
            if keep_lease && name == OsStr::new(LEASE) {
                continue;
            }
            *remaining = remaining
                .checked_sub(1)
                .ok_or_else(|| io::Error::other("temporary tree has too many entries"))?;
            match directory.child(&name, false) {
                Ok(child) => {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::MetadataExt;
                        if directory.handle().metadata()?.dev()
                            != child.handle().metadata()?.dev()
                        {
                            return Err(io::Error::other("temporary tree crosses a filesystem"));
                        }
                    }
                    children.push((name, Self::prepare(child, false, depth + 1, remaining)?));
                }
                Err(_) => {
                    // A crash after atomic hardlink publication can leave both
                    // names. Unlink them without opening or changing the file.
                    if !directory.is_regular_file(&name)? {
                        return Err(io::Error::other(
                            "temporary tree contains an unknown entry",
                        ));
                    }
                    files.push(name);
                }
            }
        }
        Ok(Self {
            directory,
            files,
            children,
        })
    }

    fn clear(self) -> io::Result<GuardedDirectory> {
        for (name, child) in self.children {
            drop(child.clear()?);
            self.directory.remove_empty_child(&name)?;
        }
        for name in self.files {
            self.directory.remove(&name)?;
        }
        Ok(self.directory)
    }
}

fn remove_instance(
    root: &GuardedDirectory,
    name: &OsStr,
    directory: GuardedDirectory,
    lease: File,
) -> io::Result<()> {
    let mut remaining = MAX_CLEANUP_ENTRIES;
    let directory = Cleanup::prepare(directory, true, 0, &mut remaining)?.clear()?;
    // On Windows the lease and directory handles intentionally deny deletion.
    // The lifecycle lock excludes a second sweeper across this close/unlink gap.
    drop(lease);
    directory.remove(OsStr::new(LEASE))?;
    drop(directory);
    root.remove_empty_child(name)?;
    root.sync()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile() -> (tempfile::TempDir, PathBuf) {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap().join("runtime");
        (temporary, root)
    }

    fn abandon(mut temporary: RuntimeTemp) -> PathBuf {
        let path = temporary.path().to_path_buf();
        // A killed process closes these OS handles without executing Drop.
        drop(temporary.lease.take());
        drop(temporary.instance.take());
        drop(temporary);
        path
    }

    #[test]
    fn normal_drop_removes_only_its_instance() {
        let (_profile, root) = profile();
        let first = RuntimeTemp::create(&root).unwrap();
        let second = RuntimeTemp::create(&root).unwrap();
        let first_path = first.path().to_path_buf();
        let second_file = second
            .publish(Path::new("resources/keep.txt"), b"keep")
            .unwrap();
        first
            .publish(Path::new("skills/example/SKILL.md"), b"skill")
            .unwrap();
        drop(first);
        assert!(!first_path.exists());
        assert_eq!(std::fs::read(second_file).unwrap(), b"keep");
        assert!(root.join("temp").join(LIFECYCLE).exists());
    }

    #[test]
    fn startup_sweeps_released_leases_but_preserves_live_instances_and_profiles() {
        let (_one, first_root) = profile();
        let (_two, second_root) = profile();
        let live = RuntimeTemp::create(&first_root).unwrap();
        let orphan = RuntimeTemp::create(&first_root).unwrap();
        orphan
            .publish(Path::new("resources/old.txt"), b"old")
            .unwrap();
        let orphan_path = abandon(orphan);
        let other = RuntimeTemp::create(&second_root).unwrap();
        let other_path = abandon(other);
        let restarted = RuntimeTemp::create(&first_root).unwrap();
        assert!(!orphan_path.exists());
        assert!(live.path().exists());
        assert!(other_path.exists());
        drop(restarted);
    }

    #[test]
    fn unknown_entries_are_not_swept_and_interrupted_empty_instances_are() {
        let (_profile, root) = profile();
        let temporary = RuntimeTemp::create(&root).unwrap();
        let unknown = temporary.root().join("user-files");
        std::fs::create_dir(&unknown).unwrap();
        std::fs::write(unknown.join("keep"), b"keep").unwrap();
        let empty = temporary
            .root()
            .join(format!("{INSTANCE_PREFIX}{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir(&empty).unwrap();
        let malformed = temporary
            .root()
            .join(format!("{INSTANCE_PREFIX}{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir(&malformed).unwrap();
        std::fs::write(malformed.join(LEASE), b"not a DL marker").unwrap();
        let interrupted = temporary
            .root()
            .join(format!("{INSTANCE_PREFIX}{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir(&interrupted).unwrap();
        std::fs::write(interrupted.join(LEASE), &MARKER[..8]).unwrap();
        let next = RuntimeTemp::create(&root).unwrap();
        assert!(!empty.exists());
        assert!(!interrupted.exists());
        assert_eq!(std::fs::read(unknown.join("keep")).unwrap(), b"keep");
        assert_eq!(
            std::fs::read(malformed.join(LEASE)).unwrap(),
            b"not a DL marker"
        );
        drop(next);
    }

    #[test]
    fn publication_is_complete_private_and_never_replaces_existing_files() {
        let (_profile, root) = profile();
        let temporary = RuntimeTemp::create(&root).unwrap();
        let content = vec![b'x'; 128 * 1024];
        let relative = Path::new("skills/example/SKILL.md");
        let path = temporary.publish(relative, &content).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), content);
        assert!(temporary.publish(relative, b"replacement").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), content);
        assert_eq!(std::fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
        for invalid in [
            "../escape",
            "skills/../../escape",
            "/absolute",
            ".lease",
            "skills/.lease",
            "skills/file:stream",
            "skills/NUL",
            "skills/COM1.txt",
            "skills/trailing.",
        ] {
            assert!(temporary.publish(Path::new(invalid), b"invalid").is_err());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for directory in [
                root.as_path(),
                temporary.root(),
                temporary.path(),
                path.parent().unwrap(),
            ] {
                assert_eq!(
                    std::fs::metadata(directory).unwrap().permissions().mode() & 0o777,
                    0o700
                );
            }
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[cfg(not(target_os = "android"))]
    #[test]
    fn interrupted_hardlink_publication_is_reclaimed_without_changing_outside_aliases() {
        let (_profile, root) = profile();
        let temporary = RuntimeTemp::create(&root).unwrap();
        let published = temporary
            .publish(Path::new("resources/complete"), b"complete")
            .unwrap();
        let staging = published.parent().unwrap().join(".dl-write-interrupted");
        std::fs::hard_link(&published, staging).unwrap();
        let outside = root.join("outside-alias");
        std::fs::hard_link(&published, &outside).unwrap();
        let stale = abandon(temporary);
        let next = RuntimeTemp::create(&root).unwrap();
        assert!(!stale.exists());
        assert_eq!(std::fs::read(outside).unwrap(), b"complete");
        drop(next);
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_preserved_and_never_traversed_during_startup_publish_or_cleanup() {
        use std::os::unix::fs::symlink;
        let (_profile, root) = profile();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("keep"), b"outside").unwrap();
        let temporary = RuntimeTemp::create(&root).unwrap();
        symlink(outside.path(), temporary.path().join("resources")).unwrap();
        assert!(temporary.publish(Path::new("resources/new"), b"new").is_err());
        let stale = abandon(temporary);
        let next = RuntimeTemp::create(&root).unwrap();
        assert!(
            std::fs::symlink_metadata(stale.join("resources"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read(outside.path().join("keep")).unwrap(), b"outside");
        assert!(!outside.path().join("new").exists());
        drop(next);
        let (_another, symlinked_root) = profile();
        std::fs::create_dir(&symlinked_root).unwrap();
        symlink(outside.path(), symlinked_root.join("temp")).unwrap();
        assert!(RuntimeTemp::create(&symlinked_root).is_err());
        assert!(
            std::fs::symlink_metadata(symlinked_root.join("temp"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[cfg(unix)]
    #[test]
    fn ancestor_replacement_cannot_redirect_drop_into_another_tree() {
        use std::os::unix::fs::symlink;
        let (_profile, root) = profile();
        let temporary = RuntimeTemp::create(&root).unwrap();
        temporary
            .publish(Path::new("resources/owned"), b"owned")
            .unwrap();
        let name = temporary.path().file_name().unwrap().to_owned();
        let original = root.join("temp");
        let moved = root.join("moved");
        std::fs::rename(&original, &moved).unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir(outside.path().join(&name)).unwrap();
        std::fs::write(outside.path().join(&name).join("keep"), b"outside").unwrap();
        symlink(outside.path(), &original).unwrap();
        drop(temporary);
        assert!(!moved.join(&name).exists());
        assert_eq!(
            std::fs::read(outside.path().join(name).join("keep")).unwrap(),
            b"outside"
        );
    }

    #[cfg(windows)]
    #[test]
    fn junctions_are_preserved_without_touching_their_target() {
        use std::os::windows::fs::MetadataExt;
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        let (_profile, root) = profile();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("keep"), b"outside").unwrap();
        let temporary = RuntimeTemp::create(&root).unwrap();
        let junction = temporary.path().join("resources");
        // A directory junction needs no symlink privilege. Only disposable paths
        // are passed; failure is a failed prerequisite, never a passing skip.
        let status = std::process::Command::new("cmd")
            .raw_arg(r#"/d /v:off /c mklink /J "%DL_TEST_JUNCTION%" "%DL_TEST_TARGET%""#)
            .env("DL_TEST_JUNCTION", &junction)
            .env("DL_TEST_TARGET", outside.path())
            .status()
            .unwrap();
        assert!(status.success(), "test requires a local NTFS junction");
        assert!(temporary.publish(Path::new("resources/new"), b"new").is_err());
        let stale = abandon(temporary);
        let next = RuntimeTemp::create(&root).unwrap();
        assert_ne!(
            std::fs::symlink_metadata(stale.join("resources"))
                .unwrap()
                .file_attributes()
                & FILE_ATTRIBUTE_REPARSE_POINT,
            0
        );
        assert_eq!(std::fs::read(outside.path().join("keep")).unwrap(), b"outside");
        assert!(!outside.path().join("new").exists());
        drop(next);
        // Remove only the fixture junction, not its target.
        std::fs::remove_dir(junction).unwrap();
    }
}
