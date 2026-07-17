use super::{LanguageAdapter, LanguageQueries};
use tree_sitter::Language;

pub struct RustAdapter;

impl RustAdapter {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Default for RustAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguageAdapter for RustAdapter {
    fn language_name(&self) -> &'static str {
        "rust"
    }

    fn extensions(&self) -> &[&'static str] {
        &["rs"]
    }

    fn language(&self) -> Language {
        tree_sitter_rust::LANGUAGE.into()
    }

    fn queries(&self) -> LanguageQueries {
        LanguageQueries {
            definitions: r"
                (function_item name: (identifier) @name) @def
                (struct_item name: (type_identifier) @name) @def
                (enum_item name: (type_identifier) @name) @def
                (impl_item type: (type_identifier) @name) @def
                (trait_item name: (type_identifier) @name) @def
            ",
            references: r"
                (call_expression function: (identifier) @ref) @call
                (call_expression function: (scoped_identifier name: (identifier) @ref)) @call
                (type_identifier) @ref
            ",
        }
    }
}
