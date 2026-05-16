use super::{LanguageAdapter, LanguageQueries};
use tree_sitter::Language;

pub struct TypeScriptAdapter;

impl TypeScriptAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl LanguageAdapter for TypeScriptAdapter {
    fn language_name(&self) -> &'static str {
        "typescript"
    }

    fn extensions(&self) -> &[&'static str] {
        &["ts"]
    }

    fn language(&self) -> Language {
        tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
    }

    fn queries(&self) -> LanguageQueries {
        Self::ts_queries()
    }
}

pub struct TsxAdapter;

impl TsxAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl LanguageAdapter for TsxAdapter {
    fn language_name(&self) -> &'static str {
        "typescript"
    }

    fn extensions(&self) -> &[&'static str] {
        &["tsx"]
    }

    fn language(&self) -> Language {
        tree_sitter_typescript::LANGUAGE_TSX.into()
    }

    fn queries(&self) -> LanguageQueries {
        TypeScriptAdapter::ts_queries()
    }
}

pub struct JavaScriptAdapter;

impl JavaScriptAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for JavaScriptAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguageAdapter for JavaScriptAdapter {
    fn language_name(&self) -> &'static str {
        "javascript"
    }

    fn extensions(&self) -> &[&'static str] {
        &["js", "jsx"]
    }

    fn language(&self) -> Language {
        tree_sitter_typescript::LANGUAGE_TSX.into()
    }

    fn queries(&self) -> LanguageQueries {
        TypeScriptAdapter::ts_queries()
    }
}

impl TypeScriptAdapter {
    pub fn ts_queries() -> LanguageQueries {
        LanguageQueries {
            definitions: r#"
                (function_declaration name: (identifier) @name) @def
                (function_signature name: (identifier) @name) @def
                (variable_declarator name: (identifier) @name value: (arrow_function)) @def
                (variable_declarator name: (identifier) @name value: (function_expression)) @def
                (class_declaration name: (type_identifier) @name) @def
                (interface_declaration name: (type_identifier) @name) @def
                (type_alias_declaration name: (type_identifier) @name) @def
                (enum_declaration name: (identifier) @name) @def
                (method_definition name: (property_identifier) @name) @def
            "#,
            references: r#"
                (call_expression function: (identifier) @ref) @call
                (call_expression function: (member_expression property: (property_identifier) @ref)) @call
                (new_expression constructor: (identifier) @ref) @call
                (type_identifier) @ref
            "#,
        }
    }
}

impl Default for TypeScriptAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl Default for TsxAdapter {
    fn default() -> Self {
        Self::new()
    }
}
