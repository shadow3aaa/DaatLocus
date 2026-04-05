use std::{
    env,
    path::{Path, PathBuf},
};

const CONFIG_DIR_NAME: &str = "config";
const STATE_DIR_NAME: &str = "state";
const CACHE_DIR_NAME: &str = "cache";
const ARTIFACTS_DIR_NAME: &str = "artifacts";
const JOURNALS_DIR_NAME: &str = "journals";
const LOGS_DIR_NAME: &str = "logs";

#[derive(Clone, Debug)]
pub struct SpinovaPaths {
    root: PathBuf,
}

impl SpinovaPaths {
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

    pub fn artifact_dir(&self, dir_name: &str) -> PathBuf {
        self.artifacts_dir().join(dir_name)
    }

    pub fn journal_file(&self, file_name: &str) -> PathBuf {
        self.journal_dir().join(file_name)
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.root.join(LOGS_DIR_NAME)
    }

    pub fn logs_file(&self, file_name: &str) -> PathBuf {
        self.logs_dir().join(file_name)
    }
}

fn resolve_spinova_home_root() -> PathBuf {
    env::var("SPINOVA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| env::home_dir().unwrap().join(".spinova"))
}

fn ensure_layout_sync(paths: &SpinovaPaths) {
    let _ = std::fs::create_dir_all(paths.root());
    let _ = std::fs::create_dir_all(paths.config_dir());
    let _ = std::fs::create_dir_all(paths.state_dir());
    let _ = std::fs::create_dir_all(paths.cache_dir());
    let _ = std::fs::create_dir_all(paths.artifacts_dir());
    let _ = std::fs::create_dir_all(paths.journal_dir());
    let _ = std::fs::create_dir_all(paths.logs_dir());
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

fn migrate_legacy_layout_sync(paths: &SpinovaPaths) {
    migrate_legacy_path_sync(paths.root.join("config.toml"), paths.config_file("config.toml"));
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
        paths.state_file("runtime_conversation"),
    );
    migrate_legacy_path_sync(
        paths.root.join("hindsight_queue"),
        paths.state_file("hindsight_queue"),
    );
    migrate_legacy_path_sync(paths.root.join("todo_board"), paths.state_file("todo_board"));
    migrate_legacy_path_sync(paths.root.join("work_state"), paths.state_file("work_state"));
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

    let legacy_spinova_toml = paths.root.join("spinova.toml");
    if legacy_spinova_toml.exists() {
        let _ = std::fs::remove_file(legacy_spinova_toml);
    }
}

pub fn spinova_paths_sync() -> SpinovaPaths {
    let root = resolve_spinova_home_root();
    let paths = SpinovaPaths::from_root(root);
    ensure_layout_sync(&paths);
    migrate_legacy_layout_sync(&paths);
    paths
}

pub async fn spinova_paths() -> SpinovaPaths {
    let root = resolve_spinova_home_root();
    let paths = SpinovaPaths::from_root(root);
    ensure_layout_sync(&paths);
    migrate_legacy_layout_sync(&paths);
    paths
}
