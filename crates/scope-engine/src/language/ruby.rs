use super::{LanguageAdapter, LanguageQueries};
use tree_sitter::Language;

pub struct RubyAdapter;

impl RubyAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for RubyAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguageAdapter for RubyAdapter {
    fn language_name(&self) -> &'static str {
        "ruby"
    }
    fn extensions(&self) -> &[&'static str] {
        &["rb"]
    }
    fn language(&self) -> Language {
        tree_sitter_ruby::LANGUAGE.into()
    }
    fn queries(&self) -> LanguageQueries {
        LanguageQueries {
            definitions: r#"
                (method name: (identifier) @name) @def
                (singleton_method name: (identifier) @name) @def
                (class name: (constant) @name) @def
                (module name: (constant) @name) @def
            "#,
            references: r#"
                (call method: (identifier) @ref) @call
                (identifier) @ref
            "#,
        }
    }
}
