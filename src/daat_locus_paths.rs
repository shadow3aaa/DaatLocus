use std::{
    env,
    path::{Path, PathBuf},
};

use crate::persistence::{PersistenceFileMode, write_bytes_atomic_sync};

const CONFIG_DIR_NAME: &str = "config";
const STATE_DIR_NAME: &str = "state";
const MEMORY_DIR_NAME: &str = "memory";
const WORKFLOWS_DIR_NAME: &str = "workflows";
const CACHE_DIR_NAME: &str = "cache";
const ARTIFACTS_DIR_NAME: &str = "artifacts";
const JOURNALS_DIR_NAME: &str = "journals";
const LOGS_DIR_NAME: &str = "logs";
const RAW_DIR_NAME: &str = "raw";
const RUNTIME_DIR_NAME: &str = "runtime";
const SESSIONS_DIR_NAME: &str = "sessions";
const CODEX_AUTH_DIR_NAME: &str = "codex-auth";

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

    pub fn workflows_dir(&self) -> PathBuf {
        self.root.join(WORKFLOWS_DIR_NAME)
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

    /// Private, Daat-owned normalized Codex OAuth imports.
    pub fn codex_auth_dir(&self) -> PathBuf {
        self.root.join(CODEX_AUTH_DIR_NAME)
    }

    pub fn codex_auth_file(&self, file_name: &str) -> PathBuf {
        self.codex_auth_dir().join(file_name)
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

    pub fn raw_dir(&self) -> PathBuf {
        self.logs_dir().join(RAW_DIR_NAME)
    }

    pub fn runtime_dir(&self) -> PathBuf {
        self.root.join(RUNTIME_DIR_NAME)
    }

    pub fn sessions_dir(&self) -> PathBuf {
        self.root.join(SESSIONS_DIR_NAME)
    }

    pub fn browser_runtime_dir(&self) -> PathBuf {
        self.runtime_dir().join("browser")
    }

    pub fn daemon_lock_file(&self) -> PathBuf {
        self.runtime_dir().join("daemon.lock")
    }

    pub fn models_dev_cache(&self) -> PathBuf {
        self.cache_dir().join("models-dev-api.json")
    }

    pub fn daemon_token_file(&self) -> PathBuf {
        self.runtime_dir().join("daemon.token")
    }

    pub fn daemon_token_registry_file(&self) -> PathBuf {
        self.runtime_dir().join("daemon_tokens.json")
    }

    pub fn browser_executable_path(&self) -> PathBuf {
        let dir = self.browser_runtime_dir();
        let path;

        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            path = dir
                .join("chrome-mac-arm64")
                .join("Google Chrome for Testing.app")
                .join("Contents")
                .join("MacOS")
                .join("Google Chrome for Testing");
        }

        #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
        {
            path = dir
                .join("chrome-mac-x64")
                .join("Google Chrome for Testing.app")
                .join("Contents")
                .join("MacOS")
                .join("Google Chrome for Testing");
        }

        #[cfg(target_os = "linux")]
        {
            path = dir.join("chrome-linux64").join("chrome");
        }

        #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
        {
            path = dir.join("chrome-win64").join("chrome.exe");
        }

        #[cfg(all(target_os = "windows", target_arch = "x86"))]
        {
            path = dir.join("chrome-win32").join("chrome.exe");
        }

        #[cfg(not(any(
            all(target_os = "macos", target_arch = "aarch64"),
            all(target_os = "macos", target_arch = "x86_64"),
            target_os = "linux",
            all(target_os = "windows", target_arch = "x86_64"),
            all(target_os = "windows", target_arch = "x86"),
        )))]
        {
            path = dir.join("chromium").join("chrome");
        }

        path
    }

    pub fn logs_file(&self, file_name: &str) -> PathBuf {
        self.logs_dir().join(file_name)
    }

    pub fn raw_file(&self, file_name: &str) -> PathBuf {
        self.raw_dir().join(file_name)
    }

    pub fn for_session(session_id: &str) -> Self {
        Self {
            root: resolve_daat_locus_home_root()
                .join(SESSIONS_DIR_NAME)
                .join(session_id),
        }
    }
}

fn resolve_daat_locus_home_root() -> PathBuf {
    env::var("DAAT_LOCUS_HOME").map_or_else(
        |_| env::home_dir().unwrap().join(".daat-locus"),
        PathBuf::from,
    )
}

fn ensure_layout_sync(paths: &DaatLocusPaths) {
    let _ = std::fs::create_dir_all(paths.config_dir());
    let _ = std::fs::create_dir_all(paths.codex_auth_dir());
    let _ = std::fs::create_dir_all(paths.state_dir());
    let _ = std::fs::create_dir_all(paths.workflows_dir());
    let _ = std::fs::create_dir_all(paths.memory_dir());
    let _ = std::fs::create_dir_all(paths.cache_dir());
    let _ = std::fs::create_dir_all(paths.artifacts_dir());
    let _ = std::fs::create_dir_all(paths.journal_dir());
    let _ = std::fs::create_dir_all(paths.logs_dir());
    let _ = std::fs::create_dir_all(paths.runtime_dir());
    let _ = std::fs::create_dir_all(paths.sessions_dir());
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

fn migrate_legacy_codex_auth_paths_sync(paths: &DaatLocusPaths) {
    let config_path = paths.config_file("config.toml");
    let Ok(text) = std::fs::read_to_string(&config_path) else {
        return;
    };
    let Ok(mut config) = toml::from_str::<toml::Value>(&text) else {
        return;
    };
    let Some(providers) = config
        .get_mut("providers")
        .and_then(toml::Value::as_table_mut)
    else {
        return;
    };

    let mut changed = false;
    for (provider_name, provider) in providers {
        let Some(provider) = provider.as_table_mut() else {
            continue;
        };
        if provider.get("type").and_then(toml::Value::as_str) != Some("openai-codex-oauth") {
            continue;
        }
        let missing_auth_file = provider
            .get("auth_file")
            .and_then(toml::Value::as_str)
            .is_none_or(|path| path.trim().is_empty());
        if !missing_auth_file {
            continue;
        }

        let component = sanitize_legacy_codex_auth_component(provider_name);
        let old_config_file = paths.config_file(&format!("openai-codex-oauth-{component}.json"));
        let migrated_file = paths.codex_auth_file(&format!("openai-codex-oauth-{component}.json"));
        if old_config_file.is_file() && !migrated_file.exists() {
            if let Some(parent) = migrated_file.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::rename(&old_config_file, &migrated_file);
        }

        let intermediate_file =
            paths.codex_auth_file(&format!("codex-auth-provider-{component}.json"));
        let auth_file = [&migrated_file, &old_config_file, &intermediate_file]
            .into_iter()
            .find(|path| path.is_file());
        let Some(auth_file) = auth_file else {
            continue;
        };

        provider.insert(
            "auth_file".to_string(),
            toml::Value::String(auth_file.display().to_string()),
        );
        changed = true;
    }

    if changed && let Ok(text) = toml::to_string_pretty(&config) {
        let _ =
            write_bytes_atomic_sync(&config_path, text.as_bytes(), PersistenceFileMode::Private);
    }
}

fn sanitize_legacy_codex_auth_component(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
        } else {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "provider".to_string()
    } else {
        trimmed.to_string()
    }
}

fn migrate_legacy_layout_sync(paths: &DaatLocusPaths) {
    migrate_legacy_path_sync(
        paths.root.join("config.toml"),
        paths.config_file("config.toml"),
    );
    migrate_legacy_path_sync(
        paths.root.join("telegram_acl.json"),
        paths.config_file("telegram_acl.json"),
    );
    migrate_legacy_codex_auth_paths_sync(paths);

    migrate_legacy_path_sync(
        paths.root.join("runtime_conversation"),
        paths.memory_file("runtime_conversation"),
    );
    migrate_legacy_path_sync(paths.root.join("todo_board"), paths.memory_file("plan"));
    migrate_legacy_path_sync(paths.root.join("plan"), paths.memory_file("plan"));
    migrate_legacy_path_sync(
        paths.state_file("runtime_conversation"),
        paths.memory_file("runtime_conversation"),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_auth_files_live_in_a_daat_owned_directory() {
        let root = PathBuf::from("/tmp/daat-locus");
        let paths = DaatLocusPaths::from_root(root.clone());

        assert_eq!(paths.codex_auth_dir(), root.join("codex-auth"));
        assert_eq!(
            paths.codex_auth_file("codex-auth-example.json"),
            root.join("codex-auth").join("codex-auth-example.json")
        );
    }

    #[test]
    fn legacy_codex_auth_file_is_migrated_and_recorded() {
        let temp = tempfile::tempdir().expect("temp dir");
        let paths = DaatLocusPaths::from_root(temp.path().to_path_buf());
        ensure_layout_sync(&paths);
        let config_path = paths.config_file("config.toml");
        std::fs::write(
            &config_path,
            r#"[providers."Codex Local"]
type = "openai-codex-oauth"
"#,
        )
        .expect("write config");
        let legacy_file = paths.config_file("openai-codex-oauth-Codex-Local.json");
        std::fs::write(&legacy_file, b"legacy tokens").expect("write legacy auth");

        migrate_legacy_codex_auth_paths_sync(&paths);

        let migrated_file = paths.codex_auth_file("openai-codex-oauth-Codex-Local.json");
        assert_eq!(
            std::fs::read(&migrated_file).expect("read migrated auth"),
            b"legacy tokens"
        );
        assert!(!legacy_file.exists());
        let config = std::fs::read_to_string(config_path).expect("read config");
        let config = toml::from_str::<toml::Value>(&config).expect("parse config");
        assert_eq!(
            config["providers"]["Codex Local"]["auth_file"].as_str(),
            Some(migrated_file.to_string_lossy().as_ref())
        );
    }

    #[test]
    fn explicit_codex_auth_file_is_preserved() {
        let temp = tempfile::tempdir().expect("temp dir");
        let paths = DaatLocusPaths::from_root(temp.path().to_path_buf());
        ensure_layout_sync(&paths);
        let config_path = paths.config_file("config.toml");
        std::fs::write(
            &config_path,
            r#"[providers.codex]
type = "openai-codex-oauth"
auth_file = "custom/auth.json"
"#,
        )
        .expect("write config");
        let legacy_file = paths.config_file("openai-codex-oauth-codex.json");
        std::fs::write(&legacy_file, b"legacy tokens").expect("write legacy auth");

        migrate_legacy_codex_auth_paths_sync(&paths);

        let config = std::fs::read_to_string(config_path).expect("read config");
        let config = toml::from_str::<toml::Value>(&config).expect("parse config");
        assert_eq!(
            config["providers"]["codex"]["auth_file"].as_str(),
            Some("custom/auth.json")
        );
        assert!(legacy_file.exists());
    }
}
