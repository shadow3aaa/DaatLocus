use std::{
    env,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

const CONFIG_DIR_NAME: &str = "config";
const STATE_DIR_NAME: &str = "state";
const CACHE_DIR_NAME: &str = "cache";
const ARTIFACTS_DIR_NAME: &str = "artifacts";
const LOGS_DIR_NAME: &str = "logs";
const RAW_DIR_NAME: &str = "raw";
const RUNTIME_DIR_NAME: &str = "runtime";
const CODEX_AUTH_DIR_NAME: &str = "codex-auth";
const OPENCODE_AUTH_DIR_NAME: &str = "opencode-auth";

static NATIVE_ROOT: OnceLock<PathBuf> = OnceLock::new();
/// The mobile application supplies its OS-private files directory before service startup.
pub fn initialize_native_root(root: PathBuf) -> Result<(), PathBuf> {
    NATIVE_ROOT.set(root)
}

#[derive(Clone, Debug)]
pub struct DaatLocusPaths {
    root: PathBuf,
}

impl DaatLocusPaths {
    pub const fn from_root(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn config_dir(&self) -> PathBuf {
        self.root.join(CONFIG_DIR_NAME)
    }

    pub fn state_dir(&self) -> PathBuf {
        self.root.join(STATE_DIR_NAME)
    }

    pub fn cache_dir(&self) -> PathBuf {
        self.root.join(CACHE_DIR_NAME)
    }

    pub fn artifacts_dir(&self) -> PathBuf {
        self.root.join(ARTIFACTS_DIR_NAME)
    }

    pub fn config_file(&self, file_name: &str) -> PathBuf {
        self.config_dir().join(file_name)
    }

    /// Private, Daat-owned normalized Codex OAuth imports.
    pub fn codex_auth_dir(&self) -> PathBuf {
        self.root.join(CODEX_AUTH_DIR_NAME)
    }

    pub fn codex_auth_file(&self, file_name: &str) -> PathBuf {
        self.codex_auth_dir().join(file_name)
    }

    /// Private, Daat-owned OpenCode Console OAuth token storage.
    pub fn opencode_auth_dir(&self) -> PathBuf {
        self.root.join(OPENCODE_AUTH_DIR_NAME)
    }

    pub fn opencode_auth_file(&self, file_name: &str) -> PathBuf {
        self.opencode_auth_dir().join(file_name)
    }

    pub fn state_file(&self, file_name: &str) -> PathBuf {
        self.state_dir().join(file_name)
    }

    pub fn artifact_dir(&self, dir_name: &str) -> PathBuf {
        self.artifacts_dir().join(dir_name)
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.root.join(LOGS_DIR_NAME)
    }

    pub fn raw_dir(&self) -> PathBuf {
        self.logs_dir().join(RAW_DIR_NAME)
    }

    pub fn runtime_dir(&self) -> PathBuf {
        self.root.join(RUNTIME_DIR_NAME)
    }

    pub fn daemon_lock_file(&self) -> PathBuf {
        self.runtime_dir().join("daemon.lock")
    }

    pub fn models_dev_cache(&self) -> PathBuf {
        self.cache_dir().join("models-dev-api.json")
    }

    pub fn logs_file(&self, file_name: &str) -> PathBuf {
        self.logs_dir().join(file_name)
    }

    pub fn raw_file(&self, file_name: &str) -> PathBuf {
        self.raw_dir().join(file_name)
    }
}

fn resolve_daat_locus_home_root() -> PathBuf {
    if let Some(root) = NATIVE_ROOT.get() {
        return root.clone();
    }
    env::var("DAAT_LOCUS_HOME").map_or_else(
        |_| env::home_dir().unwrap().join(".daat-locus"),
        PathBuf::from,
    )
}

fn ensure_layout_sync(paths: &DaatLocusPaths) {
    let _ = std::fs::create_dir_all(paths.config_dir());
    let _ = std::fs::create_dir_all(paths.codex_auth_dir());
    let _ = std::fs::create_dir_all(paths.opencode_auth_dir());
    let _ = std::fs::create_dir_all(paths.state_dir());
    let _ = std::fs::create_dir_all(paths.cache_dir());
    let _ = std::fs::create_dir_all(paths.artifacts_dir());
    let _ = std::fs::create_dir_all(paths.logs_dir());
    let _ = std::fs::create_dir_all(paths.runtime_dir());
}

fn prepared_paths(root: PathBuf) -> DaatLocusPaths {
    static PREPARED: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();
    let slot = PREPARED.get_or_init(|| Mutex::new(None));
    let mut prepared = slot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if prepared.as_ref() != Some(&root) {
        let paths = DaatLocusPaths::from_root(root.clone());
        ensure_layout_sync(&paths);
        *prepared = Some(root.clone());
    }
    DaatLocusPaths::from_root(root)
}

pub fn daat_locus_paths_sync() -> DaatLocusPaths {
    prepared_paths(resolve_daat_locus_home_root())
}

pub async fn daat_locus_paths() -> DaatLocusPaths {
    prepared_paths(resolve_daat_locus_home_root())
}

/// Serialize tests that temporarily select a private storage root.
#[cfg(test)]
pub struct DaatLocusHomeOverride {
    previous: Option<String>,
    _guard: tokio::sync::OwnedMutexGuard<()>,
}

#[cfg(test)]
impl DaatLocusHomeOverride {
    pub(crate) async fn set(path: PathBuf) -> Self {
        static LOCK: OnceLock<std::sync::Arc<tokio::sync::Mutex<()>>> = OnceLock::new();
        let guard = std::sync::Arc::clone(
            LOCK.get_or_init(|| std::sync::Arc::new(tokio::sync::Mutex::new(()))),
        )
        .lock_owned()
        .await;
        let previous = env::var("DAAT_LOCUS_HOME").ok();
        unsafe {
            env::set_var("DAAT_LOCUS_HOME", path);
        }
        Self {
            previous,
            _guard: guard,
        }
    }
}

#[cfg(test)]
impl Drop for DaatLocusHomeOverride {
    fn drop(&mut self) {
        match &self.previous {
            Some(previous) => unsafe {
                env::set_var("DAAT_LOCUS_HOME", previous);
            },
            None => unsafe {
                env::remove_var("DAAT_LOCUS_HOME");
            },
        }
    }
}
