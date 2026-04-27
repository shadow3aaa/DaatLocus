use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use miette::{IntoDiagnostic, Result};
use serde::{Serialize, de::DeserializeOwned};
use uuid::Uuid;

use crate::daat_locus_paths::{DaatLocusPaths, daat_locus_paths, daat_locus_paths_sync};

#[derive(Clone, Debug)]
pub struct PersistenceStore {
    paths: DaatLocusPaths,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PersistenceFileMode {
    Default,
    Private,
}

impl PersistenceStore {
    pub async fn runtime() -> Self {
        Self {
            paths: daat_locus_paths().await,
        }
    }

    pub fn runtime_sync() -> Self {
        Self {
            paths: daat_locus_paths_sync(),
        }
    }

    pub fn config_file(&self, file_name: &str) -> PathBuf {
        self.paths.config_file(file_name)
    }

    pub fn state_file(&self, file_name: &str) -> PathBuf {
        self.paths.state_file(file_name)
    }

    pub fn memory_file(&self, file_name: &str) -> PathBuf {
        self.paths.memory_file(file_name)
    }

    pub async fn read_postcard_state_or_default<T>(&self, file_name: &str, label: &str) -> T
    where
        T: DeserializeOwned + Default,
    {
        read_postcard_or_default(&self.state_file(file_name), label).await
    }

    pub fn read_postcard_state_or_default_sync<T>(&self, file_name: &str, label: &str) -> T
    where
        T: DeserializeOwned + Default,
    {
        read_postcard_or_default_sync(&self.state_file(file_name), label)
    }

    pub async fn read_postcard_memory<T>(&self, file_name: &str, label: &str) -> Option<T>
    where
        T: DeserializeOwned,
    {
        read_postcard_optional(&self.memory_file(file_name), label).await
    }

    pub async fn read_json_config_or_default<T>(&self, file_name: &str, label: &str) -> T
    where
        T: DeserializeOwned + Default,
    {
        read_json_or_default(&self.config_file(file_name), label).await
    }

    pub async fn read_json_memory<T>(&self, file_name: &str, label: &str) -> Option<T>
    where
        T: DeserializeOwned,
    {
        read_json_optional(&self.memory_file(file_name), label).await
    }

    pub fn read_json_file_sync<T>(&self, path: &Path, label: &str) -> Option<T>
    where
        T: DeserializeOwned,
    {
        read_json_optional_sync(path, label)
    }

    pub async fn write_postcard_memory<T>(&self, file_name: &str, value: &T) -> Result<()>
    where
        T: Serialize + ?Sized,
    {
        write_postcard_atomic(
            &self.memory_file(file_name),
            value,
            PersistenceFileMode::Default,
        )
        .await
    }

    pub async fn write_json_memory<T>(&self, file_name: &str, value: &T) -> Result<()>
    where
        T: Serialize + ?Sized,
    {
        let bytes = serde_json::to_vec_pretty(value).into_diagnostic()?;
        write_bytes_atomic(
            self.memory_file(file_name),
            bytes,
            PersistenceFileMode::Default,
        )
        .await
        .into_diagnostic()
    }

    pub fn write_json_file_sync<T>(&self, path: &Path, value: &T) -> Result<()>
    where
        T: Serialize + ?Sized,
    {
        write_json_pretty_atomic_sync(path, value, PersistenceFileMode::Default)
    }
}

pub async fn read_postcard_or_default<T>(path: &Path, label: &str) -> T
where
    T: DeserializeOwned + Default,
{
    read_postcard_optional(path, label)
        .await
        .unwrap_or_default()
}

pub async fn read_postcard_optional<T>(path: &Path, label: &str) -> Option<T>
where
    T: DeserializeOwned,
{
    let bytes = match tokio::fs::read(path).await {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return None,
        Err(err) => {
            tracing::warn!("failed to read {label} {}: {err}", path.display());
            return None;
        }
    };
    match postcard::from_bytes::<T>(&bytes) {
        Ok(value) => Some(value),
        Err(err) => {
            tracing::warn!("failed to decode {label} {}: {err}", path.display());
            quarantine_corrupt_file(path, label).await;
            None
        }
    }
}

pub fn read_postcard_or_default_sync<T>(path: &Path, label: &str) -> T
where
    T: DeserializeOwned + Default,
{
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return T::default(),
        Err(err) => {
            tracing::warn!("failed to read {label} {}: {err}", path.display());
            return T::default();
        }
    };
    match postcard::from_bytes::<T>(&bytes) {
        Ok(value) => value,
        Err(err) => {
            tracing::warn!("failed to decode {label} {}: {err}", path.display());
            quarantine_corrupt_file_sync(path, label);
            T::default()
        }
    }
}

pub async fn read_json_or_default<T>(path: &Path, label: &str) -> T
where
    T: DeserializeOwned + Default,
{
    read_json_optional(path, label).await.unwrap_or_default()
}

pub async fn read_json_optional<T>(path: &Path, label: &str) -> Option<T>
where
    T: DeserializeOwned,
{
    let bytes = match tokio::fs::read(path).await {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return None,
        Err(err) => {
            tracing::warn!("failed to read {label} {}: {err}", path.display());
            return None;
        }
    };
    match serde_json::from_slice::<T>(&bytes) {
        Ok(value) => Some(value),
        Err(err) => {
            tracing::warn!("failed to decode {label} {}: {err}", path.display());
            quarantine_corrupt_file(path, label).await;
            None
        }
    }
}

pub fn read_json_optional_sync<T>(path: &Path, label: &str) -> Option<T>
where
    T: DeserializeOwned,
{
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return None,
        Err(err) => {
            tracing::warn!("failed to read {label} {}: {err}", path.display());
            return None;
        }
    };
    match serde_json::from_slice::<T>(&bytes) {
        Ok(value) => Some(value),
        Err(err) => {
            tracing::warn!("failed to decode {label} {}: {err}", path.display());
            quarantine_corrupt_file_sync(path, label);
            None
        }
    }
}

pub async fn write_postcard_atomic<T>(
    path: &Path,
    value: &T,
    mode: PersistenceFileMode,
) -> Result<()>
where
    T: Serialize + ?Sized,
{
    let bytes = postcard::to_allocvec(value).into_diagnostic()?;
    write_bytes_atomic(path.to_path_buf(), bytes, mode)
        .await
        .into_diagnostic()
}

pub fn write_postcard_atomic_sync<T>(
    path: &Path,
    value: &T,
    mode: PersistenceFileMode,
) -> Result<()>
where
    T: Serialize + ?Sized,
{
    let bytes = postcard::to_allocvec(value).into_diagnostic()?;
    write_bytes_atomic_sync(path, &bytes, mode).into_diagnostic()
}

pub fn write_json_pretty_atomic_sync<T>(
    path: &Path,
    value: &T,
    mode: PersistenceFileMode,
) -> Result<()>
where
    T: Serialize + ?Sized,
{
    let bytes = serde_json::to_vec_pretty(value).into_diagnostic()?;
    write_bytes_atomic_sync(path, &bytes, mode).into_diagnostic()
}

pub async fn write_bytes_atomic(
    path: PathBuf,
    bytes: Vec<u8>,
    mode: PersistenceFileMode,
) -> io::Result<()> {
    tokio::task::spawn_blocking(move || write_bytes_atomic_sync(&path, &bytes, mode))
        .await
        .map_err(|err| io::Error::other(format!("atomic write task failed: {err}")))?
}

pub async fn append_bytes_durable(path: PathBuf, bytes: Vec<u8>) -> io::Result<()> {
    tokio::task::spawn_blocking(move || append_bytes_durable_sync(&path, &bytes))
        .await
        .map_err(|err| io::Error::other(format!("durable append task failed: {err}")))?
}

pub fn write_bytes_atomic_sync(
    path: &Path,
    bytes: &[u8],
    mode: PersistenceFileMode,
) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("path has no parent: {}", path.display()),
        )
    })?;
    fs::create_dir_all(parent)?;

    let temp_path = create_unique_temp_path(path);
    let write_result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)?;
        if mode == PersistenceFileMode::Private {
            set_private_file_permissions(&temp_path)?;
        }
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp_path, path)?;
        sync_parent_dir(parent)?;
        Ok(())
    })();

    if write_result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    write_result
}

pub fn append_bytes_durable_sync(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("path has no parent: {}", path.display()),
        )
    })?;
    fs::create_dir_all(parent)?;
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(bytes)?;
    file.flush()?;
    file.sync_all()?;
    drop(file);
    sync_parent_dir(parent)
}

pub fn set_private_file_permissions(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

fn create_unique_temp_path(path: &Path) -> PathBuf {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_else(|| "state".into());
    parent.join(format!(
        ".{name}.tmp-{}-{}",
        std::process::id(),
        Uuid::new_v4()
    ))
}

fn corrupt_path(path: &Path) -> PathBuf {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_else(|| "state".into());
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    parent.join(format!("{name}.corrupt-{timestamp}-{}", Uuid::new_v4()))
}

async fn quarantine_corrupt_file(path: &Path, label: &str) {
    let quarantine_path = corrupt_path(path);
    match tokio::fs::rename(path, &quarantine_path).await {
        Ok(()) => tracing::warn!(
            "moved corrupt {label} state file from {} to {}",
            path.display(),
            quarantine_path.display()
        ),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => tracing::warn!(
            "failed to move corrupt {label} state file {}: {err}",
            path.display()
        ),
    }
}

fn quarantine_corrupt_file_sync(path: &Path, label: &str) {
    let quarantine_path = corrupt_path(path);
    match fs::rename(path, &quarantine_path) {
        Ok(()) => tracing::warn!(
            "moved corrupt {label} state file from {} to {}",
            path.display(),
            quarantine_path.display()
        ),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => tracing::warn!(
            "failed to move corrupt {label} state file {}: {err}",
            path.display()
        ),
    }
}

fn sync_parent_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        fs::File::open(path)?.sync_all()?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};

    use super::*;

    #[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
    struct TestState {
        value: String,
    }

    #[test]
    fn atomic_write_replaces_complete_file() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let path = tempdir.path().join("state.bin");

        write_bytes_atomic_sync(&path, b"old", PersistenceFileMode::Default).expect("write old");
        write_bytes_atomic_sync(&path, b"new", PersistenceFileMode::Default).expect("write new");

        assert_eq!(fs::read(&path).expect("read state"), b"new");
        assert!(
            fs::read_dir(tempdir.path())
                .expect("read dir")
                .all(|entry| !entry
                    .expect("dir entry")
                    .file_name()
                    .to_string_lossy()
                    .contains(".tmp-"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn private_atomic_write_sets_private_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let tempdir = tempfile::tempdir().expect("tempdir");
        let path = tempdir.path().join("config.toml");

        write_bytes_atomic_sync(&path, b"secret", PersistenceFileMode::Private)
            .expect("write private");

        let mode = fs::metadata(&path).expect("metadata").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn corrupt_postcard_file_is_quarantined_and_defaults() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let path = tempdir.path().join("state.bin");
        fs::write(&path, b"not-postcard").expect("write corrupt");

        let state: TestState = read_postcard_or_default_sync(&path, "test state");

        assert_eq!(state, TestState::default());
        assert!(!path.exists());
        assert!(
            fs::read_dir(tempdir.path())
                .expect("read dir")
                .any(|entry| entry
                    .expect("dir entry")
                    .file_name()
                    .to_string_lossy()
                    .contains(".corrupt-"))
        );
    }

    #[test]
    fn durable_append_preserves_existing_content() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let path = tempdir.path().join("records.jsonl");

        append_bytes_durable_sync(&path, b"one\n").expect("append one");
        append_bytes_durable_sync(&path, b"two\n").expect("append two");

        assert_eq!(
            fs::read_to_string(&path).expect("read records"),
            "one\ntwo\n"
        );
    }
}
