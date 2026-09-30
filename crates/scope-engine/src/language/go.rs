use super::{LanguageAdapter, LanguageQueries};
use tree_sitter::Language;

pub struct GoAdapter;

impl GoAdapter {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Default for GoAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguageAdapter for GoAdapter {
    fn language_name(&self) -> &'static str {
        "go"
    }

    fn extensions(&self) -> &[&'static str] {
        &["go"]
    }

    fn language(&self) -> Language {
        tree_sitter_go::LANGUAGE.into()
    }

    fn queries(&self) -> LanguageQueries {
        LanguageQueries {
            definitions: r"
                (function_declaration name: (identifier) @name) @def
                (method_declaration name: (field_identifier) @name) @def
                (type_declaration (type_identifier) @name) @def
            ",
        }
    }
}
