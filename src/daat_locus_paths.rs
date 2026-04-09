use std::{
    env,
    path::{Path, PathBuf},
};

const CONFIG_DIR_NAME: &str = "config";
const STATE_DIR_NAME: &str = "state";
const MEMORY_DIR_NAME: &str = "memory";
const CACHE_DIR_NAME: &str = "cache";
const ARTIFACTS_DIR_NAME: &str = "artifacts";
const JOURNALS_DIR_NAME: &str = "journals";
const LOGS_DIR_NAME: &str = "logs";
const RUNTIME_DIR_NAME: &str = "runtime";

#[derive(Clone, Debug)]
pub struct DaatLocusPaths {
    root: PathBuf,
}

impl DaatLocusPaths {
    pub fn from_root(root: PathBuf) -> Self {
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

    pub fn memory_dir(&self) -> PathBuf {
        self.root.join(MEMORY_DIR_NAME)
    }

    pub fn cache_dir(&self) -> PathBuf {
        self.root.join(CACHE_DIR_NAME)
    }

    pub fn artifacts_dir(&self) -> PathBuf {
        self.root.join(ARTIFACTS_DIR_NAME)
    }

    pub fn journal_dir(&self) -> PathBuf {
        self.root.join(JOURNALS_DIR_NAME)
    }

    pub fn config_file(&self, file_name: &str) -> PathBuf {
        self.config_dir().join(file_name)
    }

    pub fn state_file(&self, file_name: &str) -> PathBuf {
        self.state_dir().join(file_name)
    }

    pub fn memory_file(&self, file_name: &str) -> PathBuf {
        self.memory_dir().join(file_name)
    }

    pub fn artifact_dir(&self, dir_name: &str) -> PathBuf {
        self.artifacts_dir().join(dir_name)
    }

    pub fn journal_file(&self, file_name: &str) -> PathBuf {
        self.journal_dir().join(file_name)
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.root.join(LOGS_DIR_NAME)
    }

    pub fn runtime_dir(&self) -> PathBuf {
        self.root.join(RUNTIME_DIR_NAME)
    }

    pub fn browser_runtime_dir(&self) -> PathBuf {
        self.runtime_dir().join("browser")
    }

    pub fn browser_executable_path(&self) -> PathBuf {
        #[cfg(target_os = "macos")]
        {
            #[cfg(target_arch = "aarch64")]
            {
                return self
                    .browser_runtime_dir()
                    .join("chrome-mac-arm64")
                    .join("Google Chrome for Testing.app")
                    .join("Contents")
                    .join("MacOS")
                    .join("Google Chrome for Testing");
            }

            #[cfg(target_arch = "x86_64")]
            {
                return self
                    .browser_runtime_dir()
                    .join("chrome-mac-x64")
                    .join("Google Chrome for Testing.app")
                    .join("Contents")
                    .join("MacOS")
                    .join("Google Chrome for Testing");
            }
        }

        #[cfg(target_os = "linux")]
        {
            return self
                .browser_runtime_dir()
                .join("chrome-linux64")
                .join("chrome");
        }

        #[cfg(target_os = "windows")]
        {
            #[cfg(target_arch = "x86_64")]
            {
                return self
                    .browser_runtime_dir()
                    .join("chrome-win64")
                    .join("chrome.exe");
            }

            #[cfg(target_arch = "x86")]
            {
                return self
                    .browser_runtime_dir()
                    .join("chrome-win32")
                    .join("chrome.exe");
            }
        }

        #[allow(unreachable_code)]
        self.browser_runtime_dir().join("chromium").join("chrome")
    }

    pub fn logs_file(&self, file_name: &str) -> PathBuf {
        self.logs_dir().join(file_name)
    }
}

fn resolve_daat_locus_home_root() -> PathBuf {
    env::var("DAAT_LOCUS_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| env::home_dir().unwrap().join(".daat-locus"))
}

fn ensure_layout_sync(paths: &DaatLocusPaths) {
    let _ = std::fs::create_dir_all(paths.root());
    let _ = std::fs::create_dir_all(paths.config_dir());
    let _ = std::fs::create_dir_all(paths.state_dir());
    let _ = std::fs::create_dir_all(paths.memory_dir());
    let _ = std::fs::create_dir_all(paths.cache_dir());
    let _ = std::fs::create_dir_all(paths.artifacts_dir());
    let _ = std::fs::create_dir_all(paths.journal_dir());
    let _ = std::fs::create_dir_all(paths.logs_dir());
    let _ = std::fs::create_dir_all(paths.runtime_dir());
}

fn migrate_legacy_path_sync(from: PathBuf, to: PathBuf) {
    if !from.exists() || to.exists() {
        return;
    }
    if let Some(parent) = to.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::rename(from, to);
}

fn migrate_legacy_layout_sync(paths: &DaatLocusPaths) {
    migrate_legacy_path_sync(
        paths.root.join("config.toml"),
        paths.config_file("config.toml"),
    );
    migrate_legacy_path_sync(
        paths.root.join("prompt_persona.toml"),
        paths.config_file("prompt_persona.toml"),
    );
    migrate_legacy_path_sync(
        paths.root.join("telegram_acl.json"),
        paths.config_file("telegram_acl.json"),
    );

    migrate_legacy_path_sync(
        paths.root.join("runtime_conversation"),
        paths.memory_file("runtime_conversation"),
    );
    migrate_legacy_path_sync(
        paths.root.join("hindsight_queue"),
        paths.memory_file("hindsight_queue"),
    );
    migrate_legacy_path_sync(paths.root.join("todo_board"), paths.memory_file("plan"));
    migrate_legacy_path_sync(paths.root.join("plan"), paths.memory_file("plan"));
    migrate_legacy_path_sync(
        paths.state_file("runtime_conversation"),
        paths.memory_file("runtime_conversation"),
    );
    migrate_legacy_path_sync(
        paths.state_file("hindsight_queue"),
        paths.memory_file("hindsight_queue"),
    );
    migrate_legacy_path_sync(paths.state_file("plan"), paths.memory_file("plan"));
    migrate_legacy_path_sync(paths.root.join("events"), paths.state_file("events"));
    migrate_legacy_path_sync(
        paths.root.join("pending_work_queue"),
        paths.state_file("pending_work_queue"),
    );

    migrate_legacy_path_sync(
        paths.root.join("reasoning_compiled"),
        paths.artifact_dir("reasoning_compiled"),
    );
    migrate_legacy_path_sync(
        paths.root.join("sleep_artifacts"),
        paths.artifact_dir("evaluations"),
    );
    migrate_legacy_path_sync(
        paths.root.join("evaluation_artifacts"),
        paths.artifact_dir("evaluations"),
    );
    migrate_legacy_path_sync(
        paths.artifact_dir("sleep_artifacts"),
        paths.artifact_dir("evaluations"),
    );
    migrate_legacy_path_sync(
        paths.artifact_dir("evaluation_artifacts"),
        paths.artifact_dir("evaluations"),
    );

    migrate_legacy_path_sync(
        paths.root.join("reasoning_traces.jsonl"),
        paths.journal_file("reasoning_traces.jsonl"),
    );
    migrate_legacy_path_sync(
        paths.root.join("runtime_reviews.jsonl"),
        paths.journal_file("runtime_reviews.jsonl"),
    );

    let legacy_daat_locus_toml = paths.root.join("daat-locus.toml");
    if legacy_daat_locus_toml.exists() {
        let _ = std::fs::remove_file(legacy_daat_locus_toml);
    }
}

pub fn daat_locus_paths_sync() -> DaatLocusPaths {
    let root = resolve_daat_locus_home_root();
    let paths = DaatLocusPaths::from_root(root);
    ensure_layout_sync(&paths);
    migrate_legacy_layout_sync(&paths);
    paths
}

pub async fn daat_locus_paths() -> DaatLocusPaths {
    let root = resolve_daat_locus_home_root();
    let paths = DaatLocusPaths::from_root(root);
    ensure_layout_sync(&paths);
    migrate_legacy_layout_sync(&paths);
    paths
}
