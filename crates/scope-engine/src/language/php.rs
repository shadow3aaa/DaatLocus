use super::{LanguageAdapter, LanguageQueries};
use tree_sitter::Language;

pub struct PhpAdapter;

impl PhpAdapter {
    pub fn new() -> Self { Self }
}

impl LanguageAdapter for PhpAdapter {
    fn language_name(&self) -> &'static str { "php" }
    fn extensions(&self) -> &[&'static str] { &["php"] }
    fn language(&self) -> Language { tree_sitter_php::LANGUAGE_PHP.into() }
    fn queries(&self) -> LanguageQueries {
        LanguageQueries {
            definitions: r#"
                (function_definition name: (name) @name) @def
                (class_declaration name: (name) @name) @def
                (interface_declaration name: (name) @name) @def
                (method_declaration name: (name) @name) @def
            "#,
            references: r#"
                (function_call_expression function: (name) @ref) @call
                (name) @ref
            "#,
        }
    }
}
