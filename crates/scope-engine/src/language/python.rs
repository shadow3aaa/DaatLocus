use super::{LanguageAdapter, LanguageQueries};
use tree_sitter::Language;

pub struct PythonAdapter;

impl PythonAdapter {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Default for PythonAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguageAdapter for PythonAdapter {
    fn language_name(&self) -> &'static str {
        "python"
    }

    fn extensions(&self) -> &[&'static str] {
        &["py"]
    }

    fn language(&self) -> Language {
        tree_sitter_python::LANGUAGE.into()
    }

    fn queries(&self) -> LanguageQueries {
        LanguageQueries {
            definitions: r"
                (function_definition name: (identifier) @name) @def
                (class_definition name: (identifier) @name) @def
                (decorated_definition (function_definition name: (identifier) @name)) @def
                (decorated_definition (class_definition name: (identifier) @name)) @def
            ",
            references: r"
                (call function: (identifier) @ref) @call
                (call function: (attribute attribute: (identifier) @ref)) @call
                (identifier) @ref
            ",
        }
    }
}
