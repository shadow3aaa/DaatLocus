pub mod rust;
pub mod python;
pub mod go;
pub mod typescript;

use tree_sitter::{Language, Parser};

/// Language-specific queries for tree-sitter symbol extraction.
/// Each language provides patterns for finding definitions and references.
/// Language-specific queries for tree-sitter symbol extraction.
/// Each language provides patterns for finding definitions and references.
pub struct LanguageQueries {
    pub definitions: &'static str,
    pub references: &'static str,
}

pub trait LanguageAdapter: Send + Sync {
    fn language_name(&self) -> &'static str;
    fn extensions(&self) -> &[&'static str];
    fn language(&self) -> Language;
    fn queries(&self) -> LanguageQueries;
    fn parser(&self) -> Parser {
        let mut p = Parser::new();
        p.set_language(&self.language())
            .expect("tree-sitter language init failed");
        p
    }
}

use std::collections::HashMap;

pub struct LanguageRegistry {
    adapters: Vec<Box<dyn LanguageAdapter>>,
    by_ext: HashMap<String, usize>,
}

impl LanguageRegistry {
    pub fn new() -> Self {
        let mut r = Self {
            adapters: Vec::new(),
            by_ext: HashMap::new(),
        };
        r.register(Box::new(rust::RustAdapter::new()));
        r.register(Box::new(python::PythonAdapter::new()));
        r.register(Box::new(go::GoAdapter::new()));
        r.register(Box::new(typescript::TypeScriptAdapter::new()));
        r.register(Box::new(typescript::JavaScriptAdapter::new()));
        r
    }

    pub fn register(&mut self, adapter: Box<dyn LanguageAdapter>) {
        let idx = self.adapters.len();
        for ext in adapter.extensions() {
            self.by_ext.insert(ext.to_string(), idx);
        }
        self.adapters.push(adapter);
    }

    pub fn get(&self, ext: &str) -> Option<&dyn LanguageAdapter> {
        let idx = self.by_ext.get(ext)?;
        Some(self.adapters[*idx].as_ref())
    }
}

impl Default for LanguageRegistry {
    fn default() -> Self {
        Self::new()
    }
}
