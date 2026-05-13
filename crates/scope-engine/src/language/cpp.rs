use super::{LanguageAdapter, LanguageQueries};
use tree_sitter::Language;

pub struct CppAdapter;

impl CppAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CppAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguageAdapter for CppAdapter {
    fn language_name(&self) -> &'static str {
        "cpp"
    }
    fn extensions(&self) -> &[&'static str] {
        &["cpp", "cxx", "cc", "hpp", "hxx", "hh"]
    }
    fn language(&self) -> Language {
        tree_sitter_cpp::LANGUAGE.into()
    }
    fn queries(&self) -> LanguageQueries {
        LanguageQueries {
            definitions: r#"
                (function_definition declarator: (function_declarator declarator: (identifier) @name)) @def
                (class_specifier name: (type_identifier) @name) @def
                (struct_specifier name: (type_identifier) @name) @def
                (declaration declarator: (init_declarator declarator: (identifier) @name)) @def
            "#,
            references: r#"
                (call_expression function: (identifier) @ref) @call
                (call_expression function: (field_identifier) @ref) @call
                (type_identifier) @ref
            "#,
        }
    }
}
