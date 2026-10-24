//! Owner-local browser library. Paths never come from webpage JavaScript.
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Default, Serialize, Deserialize)]
pub(crate) struct Library {
    pub history: Vec<Visit>,
    pub downloads: Vec<Download>,
}
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Visit {
    pub id: String,
    pub url: String,
    pub title: String,
    pub time: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub favicon: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Download {
    pub id: String,
    pub url: String,
    pub name: String,
    pub time: u64,
    pub status: String,
    pub bytes: u64,
}
pub(crate) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
impl Library {
    pub fn load(root: &Path) -> Self {
        let mut value: Self = fs::read(root.join("runtime/browser-library.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        for download in &mut value.downloads {
            if download.status == "downloading" {
                download.status = "interrupted".into();
            }
        }
        value
    }
    pub fn save(&self, root: &Path) -> Result<(), String> {
        let directory = root.join("runtime");
        fs::create_dir_all(&directory).map_err(|_| "Browser library unavailable")?;
        let temporary = directory.join("browser-library.pending");
        fs::write(
            &temporary,
            serde_json::to_vec(self).map_err(|_| "Browser library unavailable")?,
        )
        .map_err(|_| "Browser library could not be saved")?;
        fs::rename(temporary, directory.join("browser-library.json"))
            .map_err(|_| "Browser library could not be saved".into())
    }
    pub fn visit(&mut self, url: &str, title: &str) {
        if let Some(last) = self.history.last_mut()
            && last.url == url
        {
            last.title = title.into();
            last.time = now();
            return;
        }
        self.history.push(Visit {
            id: uuid::Uuid::new_v4().simple().to_string(),
            url: url.into(),
            title: title.into(),
            time: now(),
            favicon: self
                .history
                .iter()
                .rev()
                .find(|entry| {
                    url::Url::parse(&entry.url)
                        .ok()
                        .zip(url::Url::parse(url).ok())
                        .is_some_and(|(a, b)| a.origin() == b.origin())
                })
                .and_then(|entry| entry.favicon.clone()),
        });
    }
}
pub(crate) fn download_path(root: &Path, download: &Download) -> PathBuf {
    root.join("runtime")
        .join("browser-downloads")
        .join(&download.id)
        .join(&download.name)
}
pub(crate) fn safe_name(path: &Path) -> String {
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("download");
    let clean: String = name
        .chars()
        .map(|c| {
            if c.is_control() || "<>:\"/\\|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .take(180)
        .collect();
    let clean = clean.trim_matches(['.', ' ']);
    let stem = clean
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    if clean.is_empty() {
        "download".into()
    } else if matches!(
        stem.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    ) {
        format!("file-{clean}")
    } else {
        clean.into()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn library_recovers_interrupted_downloads_and_keeps_visits() {
        let root = tempfile::tempdir().unwrap();
        let mut library = Library::default();
        library.visit("https://example.com/one", "One");
        library.visit("https://example.com/two", "Two");
        library.downloads.push(Download {
            id: "a".repeat(32),
            url: "https://example.com/file".into(),
            name: safe_name(Path::new("CON")),
            time: now(),
            status: "downloading".into(),
            bytes: 0,
        });
        library.save(root.path()).unwrap();
        let recovered = Library::load(root.path());
        assert_eq!(
            recovered
                .history
                .iter()
                .map(|v| v.title.as_str())
                .collect::<Vec<_>>(),
            ["One", "Two"]
        );
        assert_eq!(recovered.downloads[0].status, "interrupted");
    }
}
