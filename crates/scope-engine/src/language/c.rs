use super::{LanguageAdapter, LanguageQueries};
use tree_sitter::Language;

pub struct CAdapter;

impl CAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguageAdapter for CAdapter {
    fn language_name(&self) -> &'static str {
        "c"
    }
    fn extensions(&self) -> &[&'static str] {
        &["c", "h"]
    }
    fn language(&self) -> Language {
        tree_sitter_c::LANGUAGE.into()
    }
    fn queries(&self) -> LanguageQueries {
        LanguageQueries {
            definitions: r#"
                (function_definition declarator: (function_declarator declarator: (identifier) @name)) @def
                (declaration declarator: (init_declarator declarator: (identifier) @name)) @def
            "#,
            references: r#"
                (call_expression function: (identifier) @ref) @call
                (identifier) @ref
            "#,
        }
    }
}
