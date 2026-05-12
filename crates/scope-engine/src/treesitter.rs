use std::path::Path;

use crate::analyzer::Analyzer;
use crate::api::AffectedSelector;
use std::collections::HashSet;

use crate::language::LanguageRegistry;

pub struct TreeSitterAnalyzer {
    registry: LanguageRegistry,
}

impl TreeSitterAnalyzer {
    pub fn new() -> Self {
        Self {
            registry: LanguageRegistry::new(),
        }
    }

    /// Given a file path and a 1-based line number, find the innermost
    /// named definition (function, struct, enum, trait, impl) that contains
    /// that line. Returns a CodeStruct-style selector like
    /// `src/foo.rs::fn authenticate`.
    pub fn find_containing_symbol(
        &self,
        file_path: &Path,
        line_number: usize,
        project_root: &Path,
    ) -> Option<String> {
        let ext = file_path.extension()?.to_str()?;
        let adapter = self.registry.get(ext)?;

        let content = std::fs::read_to_string(file_path).ok()?;
        let mut parser = adapter.parser();
        let tree = parser.parse(&content, None)?;
        let root_node = tree.root_node();

        // Walk the tree looking for the deepest definition containing line_number
        let _cursor = root_node.walk();
        let mut best_name: Option<String> = None;
        let mut best_kind: Option<String> = None;

        // Collect all definition nodes that contain this line, pick deepest
        self.collect_containing_defs(
            root_node,
            &content,
            line_number,
            &mut best_name,
            &mut best_kind,
        );

        let name = best_name?;
        let kind_prefix = match best_kind.as_deref() {
            Some("function_item") => "fn ",
            Some("struct_item") => "struct ",
            Some("enum_item") => "enum ",
            Some("trait_item") => "trait ",
            Some("impl_item") => "impl ",
            _ => "",
        };

        // Build relative path
        let rel_path = file_path
            .strip_prefix(project_root)
            .ok()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| file_path.to_string_lossy().to_string());

        Some(format!("{rel_path}::{kind_prefix}{name}"))
    }

    fn collect_containing_defs(
        &self,
        node: tree_sitter::Node,
        source: &str,
        target_line: usize,
        best_name: &mut Option<String>,
        best_kind: &mut Option<String>,
    ) {
        let start = node.start_position();
        let end = node.end_position();
        let node_start_line = start.row + 1; // tree-sitter rows are 0-based
        let node_end_line = end.row + 1;

        if target_line < node_start_line || target_line > node_end_line {
            return;
        }

        let kind = node.kind();
        // Check if this is a named definition
        let is_def = matches!(
            kind,
            "function_item"
                | "struct_item"
                | "enum_item"
                | "trait_item"
                | "impl_item"
        );

        if is_def {
            // Extract the name — it's the first child identifier
            if let Some(name) = self.extract_def_name(node, source) {
                *best_name = Some(name);
                *best_kind = Some(kind.to_string());
            }
        }

        // Recurse into children — deeper wins
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                self.collect_containing_defs(child, source, target_line, best_name, best_kind);
            }
        }
    }

    fn extract_def_name(&self, node: tree_sitter::Node, source: &str) -> Option<String> {
        for i in 0..node.child_count() {
            let child = node.child(i)?;
            let kind = child.kind();
            if kind == "identifier" || kind == "type_identifier" {
                return child.utf8_text(source.as_bytes()).ok().map(|s| s.to_string());
            }
        }
        None
    }

    /// Find all selectors in the same file that reference a given symbol name.
    ///
    /// Walk the CST for identifiers / type_identifiers matching `symbol_name`.
    /// For each match, determine its containing definition via find_containing_symbol,
    /// producing selectors like `src/foo.rs::fn startup()`.
    ///
    /// Excludes definition nodes (function_item, struct_item, etc.) to avoid
    /// self-referencing.
    pub fn find_referencing_symbols(
        &self,
        file_path: &Path,
        symbol_name: &str,
        project_root: &Path,
    ) -> Vec<AffectedSelector> {
        let ext = match file_path.extension().and_then(|e| e.to_str()) {
            Some(e) => e,
            None => return vec![],
        };
        let adapter = match self.registry.get(ext) {
            Some(a) => a,
            None => return vec![],
        };
        let content = match std::fs::read_to_string(file_path) {
            Ok(c) => c,
            Err(_) => return vec![],
        };
        let mut parser = adapter.parser();
        let tree = match parser.parse(&content, None) {
            Some(t) => t,
            None => return vec![],
        };

        let mut results: Vec<AffectedSelector> = Vec::new();
        let mut seen = HashSet::new();
        self.collect_refs(
            tree.root_node(),
            &content,
            symbol_name,
            file_path,
            project_root,
            &mut results,
            &mut seen,
        );
        results
    }

    fn collect_refs(
        &self,
        node: tree_sitter::Node,
        source: &str,
        target_name: &str,
        file_path: &Path,
        project_root: &Path,
        results: &mut Vec<AffectedSelector>,
        seen: &mut HashSet<String>,
    ) {
        if (node.kind() == "identifier" || node.kind() == "type_identifier")
            && node.utf8_text(source.as_bytes()).map_or(false, |s| s == target_name)
        {
            if let Some(parent) = node.parent() {
                let parent_kind = parent.kind();
                let is_def = matches!(
                    parent_kind,
                    "function_item" | "struct_item" | "enum_item" | "trait_item" | "impl_item"
                );
                if !is_def {
                    let line = node.start_position().row + 1;
                    if let Some(sel) = self.find_containing_symbol(file_path, line, project_root) {
                        if seen.insert(sel.clone()) {
                            results.push(AffectedSelector {
                                selector: sel,
                                reason: format!("references \"{}\" at line {}", target_name, line),
                            });
                        }
                    }
                }
            }
        }

        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                self.collect_refs(
                    child, source, target_name, file_path, project_root, results, seen,
                );
            }
        }
    }

}

impl Analyzer for TreeSitterAnalyzer {
    fn find_references(&self, _selector: &str) -> Vec<AffectedSelector> {
        vec![]
    }
    fn find_callers(&self, _selector: &str) -> Vec<AffectedSelector> {
        vec![]
    }
    fn find_definition(&self, _selector: &str) -> Option<AffectedSelector> {
        None
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;

    fn write_temp_rust_file(dir: &Path, name: &str, content: &str) -> PathBuf {
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        path
    }

    const RUST_CODE: &str = "// line 1\n                             // line 2\n                             const VERSION: &str = \"0.1\";\n                             \n                             pub fn authenticate(token: &str) -> bool {\n                                 // line 6\n                                 token.len() > 0\n                             }\n                             \n                             pub struct Config {\n                                 pub timeout_ms: u64,\n                             }\n                             \n                             impl Config {\n                                 pub fn default_timeout() -> u64 {\n                                     // line 16\n                                     5000\n                                 }\n                             }\n";

    #[test]
    fn find_fn_by_line() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let file_path = write_temp_rust_file(dir, "auth.rs", RUST_CODE);

        let analyzer = TreeSitterAnalyzer::new();
        let sel = analyzer.find_containing_symbol(&file_path, 6, dir);
        assert_eq!(sel, Some("auth.rs::fn authenticate".to_string()));
    }

    #[test]
    fn find_struct_by_line() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let file_path = write_temp_rust_file(dir, "auth.rs", RUST_CODE);

        let analyzer = TreeSitterAnalyzer::new();
        let sel = analyzer.find_containing_symbol(&file_path, 11, dir);
        assert_eq!(sel, Some("auth.rs::struct Config".to_string()));
    }

    #[test]
    fn find_impl_by_line() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let file_path = write_temp_rust_file(dir, "auth.rs", RUST_CODE);

        let analyzer = TreeSitterAnalyzer::new();
        // line 16 is inside impl Config -> fn default_timeout (deepest wins)
        let sel = analyzer.find_containing_symbol(&file_path, 16, dir);
        assert_eq!(sel, Some("auth.rs::fn default_timeout".to_string()));
    }

    #[test]
    fn outside_all_defs_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let file_path = write_temp_rust_file(dir, "auth.rs", RUST_CODE);

        let analyzer = TreeSitterAnalyzer::new();
        let sel = analyzer.find_containing_symbol(&file_path, 1, dir);
        assert_eq!(sel, None);
    }

    #[test]
    fn missing_file_returns_none() {
        let analyzer = TreeSitterAnalyzer::new();
        let sel = analyzer.find_containing_symbol(
            Path::new("/nonexistent/file.rs"),
            1,
            Path::new("/"),
        );
        assert_eq!(sel, None);
    }
}
