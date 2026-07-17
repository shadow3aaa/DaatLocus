use super::{LanguageAdapter, LanguageQueries};
use tree_sitter::Language;

pub struct JavaAdapter;

impl JavaAdapter {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Default for JavaAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguageAdapter for JavaAdapter {
    fn language_name(&self) -> &'static str {
        "java"
    }

    fn extensions(&self) -> &[&'static str] {
        &["java"]
    }

    fn language(&self) -> Language {
        tree_sitter_java::LANGUAGE.into()
    }

    fn queries(&self) -> LanguageQueries {
        LanguageQueries {
            definitions: r"
                (method_declaration name: (identifier) @name) @def
                (class_declaration name: (identifier) @name) @def
                (interface_declaration name: (identifier) @name) @def
                (enum_declaration name: (identifier) @name) @def
                (constructor_declaration name: (identifier) @name) @def
                (field_declaration declarator: (variable_declarator name: (identifier) @name)) @def
                (local_variable_declaration declarator: (variable_declarator name: (identifier) @name)) @def
            ",
            references: r"
                (method_invocation name: (identifier) @ref) @call
                (object_creation_expression type: (type_identifier) @ref) @call
                (type_identifier) @ref
                (identifier) @ref
            ",
        }
    }
}
