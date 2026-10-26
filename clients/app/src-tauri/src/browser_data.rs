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
    #[serde(default)]
    pub bookmarks: Vec<Visit>,
    #[serde(default)]
    pub searches: Vec<Search>,
}
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Search {
    pub id: String,
    pub query: String,
    pub time: u64,
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
    /// Owner-only library queries return bounded source records, never rendered rows.
    pub fn saved(
        &mut self,
        action: &str,
        input: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        use serde_json::json;
        let query = input["query"].as_str().unwrap_or("").trim();
        if query.len() > 4096 {
            return Err("Search is too long".into());
        }
        let needle = query.to_lowercase();
        let matches = |title: &str, url: &str| {
            let text = format!("{title} {url}").to_lowercase();
            needle.split_whitespace().all(|word| text.contains(word))
        };
        let id = input["id"].as_str();
        match action {
            "suggestions" => {
                let mut seen = std::collections::HashSet::new();
                let history: Vec<_> = self
                    .history
                    .iter()
                    .rev()
                    .filter(|entry| matches(&entry.title, &entry.url) && seen.insert(&entry.url))
                    .take(8)
                    .collect();
                Ok(
                    json!({"history":history,"bookmarks":self.bookmarks.iter().rev().filter(|entry|matches(&entry.title,&entry.url)).take(8).collect::<Vec<_>>(),"searches":self.searches.iter().rev().filter(|entry|matches(&entry.query,"")).take(8).collect::<Vec<_>>()}),
                )
            }
            "bookmarks" => {
                let offset = input["offset"].as_u64().unwrap_or(0) as usize;
                let items: Vec<_> = self
                    .bookmarks
                    .iter()
                    .rev()
                    .filter(|entry| matches(&entry.title, &entry.url))
                    .skip(offset)
                    .take(101)
                    .collect();
                Ok(
                    json!({"items":items.iter().take(100).collect::<Vec<_>>(),"next_offset":if items.len()>100{Some(offset.saturating_add(100))}else{None}}),
                )
            }
            "bookmark" => Ok(
                json!({"item":self.bookmarks.iter().find(|entry|Some(entry.id.as_str())==id||input["url"].as_str()==Some(entry.url.as_str()))}),
            ),
            "save_bookmark" => {
                let url = url::Url::parse(input["url"].as_str().ok_or("Enter a URL")?)
                    .map_err(|_| "Invalid bookmark URL")?;
                if !matches!(url.scheme(), "http" | "https")
                    || url.host_str().is_none()
                    || !url.username().is_empty()
                    || url.password().is_some()
                {
                    return Err("Invalid bookmark URL".into());
                }
                let default_title = url.host_str().ok_or("Invalid bookmark URL")?.to_owned();
                let origin = url.origin();
                let url = url.to_string();
                let title = input["title"].as_str().unwrap_or("").trim();
                if title.len() > 4096 {
                    return Err("Bookmark name is too long".into());
                }
                if id.is_some()
                    && !self
                        .bookmarks
                        .iter()
                        .any(|entry| Some(entry.id.as_str()) == id)
                {
                    return Err("Bookmark no longer exists".into());
                }
                if id.is_some()
                    && self
                        .bookmarks
                        .iter()
                        .any(|entry| entry.url == url && Some(entry.id.as_str()) != id)
                {
                    return Err("This page is already bookmarked".into());
                }
                let previous = self
                    .bookmarks
                    .iter()
                    .find(|entry| id.map_or(entry.url == url, |id| entry.id == id));
                let entry = Visit {
                    id: previous
                        .map(|entry| entry.id.clone())
                        .unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string()),
                    url,
                    title: if title.is_empty() {
                        default_title
                    } else {
                        title.into()
                    },
                    time: now(),
                    favicon: input["favicon"]
                        .as_str()
                        .filter(|icon| {
                            icon.starts_with("data:image/png;base64,") && icon.len() < 48_000
                        })
                        .map(str::to_owned)
                        .or_else(|| {
                            previous
                                .filter(|entry| {
                                    url::Url::parse(&entry.url)
                                        .is_ok_and(|old| old.origin() == origin)
                                })
                                .and_then(|entry| entry.favicon.clone())
                        }),
                };
                self.bookmarks.retain(|old| old.id != entry.id);
                self.bookmarks.push(entry.clone());
                Ok(json!(entry))
            }
            "remove_bookmark" => {
                self.bookmarks.retain(|entry| Some(entry.id.as_str()) != id);
                Ok(json!({}))
            }
            "record_search" => {
                if query.is_empty() {
                    return Err("Enter a search".into());
                }
                self.searches
                    .retain(|entry| entry.query.to_lowercase() != needle);
                self.searches.push(Search {
                    id: uuid::Uuid::new_v4().simple().to_string(),
                    query: query.into(),
                    time: now(),
                });
                Ok(json!({}))
            }
            "remove_search" => {
                self.searches.retain(|entry| Some(entry.id.as_str()) != id);
                Ok(json!({}))
            }
            _ => Err("Unknown browser library operation".into()),
        }
    }
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
    fn bookmarks_survive_reload_edit_and_removal_without_clearing_visits() {
        use serde_json::json;
        let root = tempfile::tempdir().unwrap();
        let mut library = Library::default();
        library.visit("https://example.com/history", "History");
        let icon = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Wl6sVcAAAAASUVORK5CYII=";
        let first = library
            .saved(
                "save_bookmark",
                json!({"url":"https://example.com","title":"Original","favicon":icon}),
            )
            .unwrap();
        let duplicate = library
            .saved(
                "save_bookmark",
                json!({"url":"https://example.com/","title":"Changed"}),
            )
            .unwrap();
        assert_eq!(first["id"], duplicate["id"]);
        assert_eq!(library.bookmarks.len(), 1);
        library.save(root.path()).unwrap();
        let mut library = Library::load(root.path());
        let loaded = library
            .saved("bookmark", json!({"url":"https://example.com/"}))
            .unwrap();
        assert_eq!(loaded["item"]["title"], "Changed");
        assert_eq!(loaded["item"]["favicon"], icon);
        library
            .saved(
                "save_bookmark",
                json!({"id":first["id"],"url":"https://example.org/docs","title":"Documentation"}),
            )
            .unwrap();
        library.save(root.path()).unwrap();
        let mut library = Library::load(root.path());
        assert_eq!(library.bookmarks[0].url, "https://example.org/docs");
        assert!(
            library.bookmarks[0].favicon.is_none(),
            "a changed site must not inherit the previous site's icon"
        );
        assert!(
            library
                .saved("save_bookmark", json!({"url":"javascript:alert(1)"}))
                .is_err()
        );
        assert!(
            library
                .saved(
                    "save_bookmark",
                    json!({"url":"https://user:secret@example.com"})
                )
                .is_err()
        );
        library
            .saved("remove_bookmark", json!({"id":first["id"]}))
            .unwrap();
        library.save(root.path()).unwrap();
        let library = Library::load(root.path());
        assert!(library.bookmarks.is_empty());
        assert_eq!(library.history[0].title, "History");
    }
    #[test]
    fn suggestions_search_the_entire_library_and_retain_unique_recent_searches() {
        use serde_json::json;
        let mut library = Library::default();
        library.visit("https://example.com/old", "An older reference");
        for i in 0..130 {
            library.visit(&format!("https://example.com/{i}"), "Recent page");
        }
        library
            .saved("record_search", json!({"query":"WebView guide"}))
            .unwrap();
        library
            .saved("record_search", json!({"query":"webview guide"}))
            .unwrap();
        assert_eq!(library.searches.len(), 1);
        let found = library
            .saved("suggestions", json!({"query":"older reference"}))
            .unwrap();
        assert_eq!(found["history"][0]["url"], "https://example.com/old");
        let bounded = library.saved("suggestions", json!({"query":""})).unwrap();
        assert_eq!(bounded["history"].as_array().unwrap().len(), 8);
        assert_eq!(bounded["searches"][0]["query"], "webview guide");
        let id = bounded["searches"][0]["id"].clone();
        library.saved("remove_search", json!({"id":id})).unwrap();
        assert!(library.searches.is_empty());
    }
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
