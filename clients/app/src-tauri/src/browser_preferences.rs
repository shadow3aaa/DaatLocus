//! Device-local preferences for the shared browser, independent of account settings.
use serde::{Deserialize, Serialize};
use std::path::Path;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Preferences {
    pub search_engine: String,
    pub language: String,
    pub default_zoom: f64,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            search_engine: "google".into(),
            language: "system".into(),
            default_zoom: 1.0,
        }
    }
}
impl Preferences {
    pub fn validate(&self) -> bool {
        matches!(
            self.search_engine.as_str(),
            "google" | "bing" | "duckduckgo" | "baidu"
        ) && matches!(self.language.as_str(), "system" | "zh-CN" | "en-US")
            && self.default_zoom.is_finite()
            && (0.5..=2.0).contains(&self.default_zoom)
    }
    pub fn load(root: &Path) -> Self {
        std::fs::read(root.join("runtime/browser-preferences.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Self>(&bytes).ok())
            .filter(Self::validate)
            .unwrap_or_default()
    }
    pub fn patch(&self, root: &Path, patch: serde_json::Value) -> Result<Self, String> {
        let mut value = serde_json::to_value(self).map_err(|_| "Browser settings unavailable")?;
        for (key, item) in patch.as_object().ok_or("Invalid browser settings")? {
            if !matches!(key.as_str(), "search_engine" | "language" | "default_zoom") {
                return Err("Unknown browser setting".into());
            }
            value[key] = item.clone();
        }
        let next: Self = serde_json::from_value(value).map_err(|_| "Invalid browser settings")?;
        if !next.validate() {
            return Err("Invalid browser settings".into());
        }
        let directory = root.join("runtime");
        std::fs::create_dir_all(&directory).map_err(|_| "Browser settings could not be saved")?;
        let pending = directory.join("browser-preferences.pending");
        std::fs::write(
            &pending,
            serde_json::to_vec(&next).map_err(|_| "Invalid browser settings")?,
        )
        .map_err(|_| "Browser settings could not be saved")?;
        std::fs::rename(pending, directory.join("browser-preferences.json"))
            .map_err(|_| "Browser settings could not be saved")?;
        Ok(next)
    }
    pub fn locale(&self) -> String {
        if self.language == "system" {
            sys_locale::get_locale()
                .unwrap_or_else(|| "en-US".into())
                .replace('_', "-")
        } else {
            self.language.clone()
        }
    }
    #[cfg(target_os = "macos")]
    pub fn language_script(&self) -> String {
        let locale = serde_json::to_string(&self.locale()).unwrap();
        format!(
            "/*dl-browser-language*/(()=>{{const locale={locale};for(const [key,value] of [['language',locale],['languages',[locale,locale.split('-')[0]]]]){{Object.defineProperty(Navigator.prototype,key,{{get:()=>value,configurable:true}})}}}})()"
        )
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn patches_preserve_unedited_settings_and_reject_invalid_candidates() {
        let root = tempfile::tempdir().unwrap();
        let prefs = Preferences::default()
            .patch(
                root.path(),
                serde_json::json!({"language":"zh-CN","default_zoom":1.25}),
            )
            .unwrap();
        prefs
            .patch(root.path(), serde_json::json!({"search_engine":"bing"}))
            .unwrap();
        let saved = Preferences::load(root.path());
        assert_eq!(saved.language, "zh-CN");
        assert_eq!(saved.search_engine, "bing");
        assert_eq!(saved.default_zoom, 1.25);
        assert!(
            saved
                .patch(root.path(), serde_json::json!({"language":"invalid"}))
                .is_err()
        );
        assert_eq!(Preferences::load(root.path()).language, "zh-CN");
    }
}
