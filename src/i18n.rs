use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum Locale {
    #[default]
    #[serde(rename = "en-US")]
    EnUs,
    #[serde(rename = "zh-CN")]
    ZhCn,
}

impl Locale {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::EnUs => "en-US",
            Self::ZhCn => "zh-CN",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::EnUs => "English",
            Self::ZhCn => "简体中文",
        }
    }

    pub fn from_language_setup_index(index: usize) -> Self {
        match index {
            1 => Self::ZhCn,
            _ => Self::EnUs,
        }
    }
}

#[macro_export]
macro_rules! tr {
    ($locale:expr, $key:literal) => {
        ::rust_i18n::t!($key, locale = $locale.as_str()).to_string()
    };
    ($locale:expr, $key:literal, $($arg:ident = $value:expr),+ $(,)?) => {
        ::rust_i18n::t!($key, locale = $locale.as_str(), $($arg = $value),+).to_string()
    };
}
