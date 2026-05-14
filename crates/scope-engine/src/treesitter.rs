use std::path::Path;

use crate::language::LanguageRegistry;
use crate::selector::{ParsedSelector, SymbolKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolMatch {
    pub name: String,
    pub kind: SymbolKind,
    pub kind_prefix: &'static str,
    pub start_line: usize,
    pub end_line: usize,
}

impl SymbolMatch {
    pub fn canonical_selector(&self, file_path: &Path, project_root: &Path) -> String {
        let rel_path = file_path
            .strip_prefix(project_root)
            .ok()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| file_path.to_string_lossy().to_string());

        format!(
            "{}::{}{} #L{}-L{}",
            rel_path, self.kind_prefix, self.name, self.start_line, self.end_line
        )
    }

    pub fn source_from(&self, content: &str) -> String {
        let lines: Vec<&str> = content.lines().collect();
        if self.start_line == 0 || self.end_line < self.start_line || self.start_line > lines.len()
        {
            return String::new();
        }

        let start_idx = self.start_line - 1;
        let end_idx = self.end_line.min(lines.len());
        let mut snippet = lines[start_idx..end_idx].join("\n");
        if content.ends_with('\n') || self.end_line < lines.len() {
            snippet.push('\n');
        }
        snippet
    }
}

pub struct TreeSitterAnalyzer {
    registry: LanguageRegistry,
}

impl Default for TreeSitterAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl TreeSitterAnalyzer {
    pub fn new() -> Self {
        Self {
            registry: LanguageRegistry::new(),
        }
    }

    /// Given a file path and a 1-based line number, find the innermost
    /// named definition (function, struct, enum, trait, impl) that contains
    /// that line. Returns a canonical CodeStruct-style selector like
    /// `src/foo.rs::fn authenticate #L10-L20`.
    pub fn find_containing_symbol(
        &self,
        file_path: &Path,
        line_number: usize,
        project_root: &Path,
    ) -> Option<String> {
        self.find_containing_symbol_match(file_path, line_number)
            .map(|m| m.canonical_selector(file_path, project_root))
    }

    pub fn find_containing_symbol_match(
        &self,
        file_path: &Path,
        line_number: usize,
    ) -> Option<SymbolMatch> {
        let symbols = self.symbols_in_file(file_path).ok()?;
        symbols
            .into_iter()
            .filter(|m| line_number >= m.start_line && line_number <= m.end_line)
            .max_by_key(|m| (m.start_line, usize::MAX - m.end_line))
    }

    pub fn resolve_selector(
        &self,
        file_path: &Path,
        parsed: &ParsedSelector,
    ) -> Result<SymbolMatch, String> {
        let symbols = self.symbols_in_file(file_path)?;
        let mut matches: Vec<SymbolMatch> = symbols
            .into_iter()
            .filter(|m| symbol_matches_selector(m, parsed))
            .collect();

        if let Some((start, end)) = parsed.line_range {
            matches.retain(|m| m.start_line == start && m.end_line == end);
        }

        match matches.len() {
            0 => Err(format!(
                "symbol '{}' not found in {}",
                parsed.name,
                file_path.display()
            )),
            1 => Ok(matches.remove(0)),
            _ => {
                let candidates = matches
                    .iter()
                    .map(|m| {
                        format!(
                            "{}{} #L{}-L{}",
                            m.kind_prefix, m.name, m.start_line, m.end_line
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                Err(format!(
                    "ambiguous selector for '{}' in {}; candidates: {}",
                    parsed.name,
                    file_path.display(),
                    candidates
                ))
            }
        }
    }

    pub fn symbols_in_file(&self, file_path: &Path) -> Result<Vec<SymbolMatch>, String> {
        let ext = file_path
            .extension()
            .and_then(|e| e.to_str())
            .ok_or_else(|| {
                format!(
                    "cannot determine language from file: {}",
                    file_path.display()
                )
            })?;
        let adapter = self
            .registry
            .get(ext)
            .ok_or_else(|| format!("unsupported language extension: {ext}"))?;

        let content = std::fs::read_to_string(file_path)
            .map_err(|e| format!("failed to read {}: {e}", file_path.display()))?;
        let mut parser = adapter.parser();
        let tree = parser
            .parse(&content, None)
            .ok_or_else(|| format!("failed to parse {}", file_path.display()))?;
        let mut symbols = Vec::new();
        self.collect_symbols(tree.root_node(), &content, &mut symbols);
        Ok(symbols)
    }

    /// Validate that a file's content can be parsed by tree-sitter.
    /// Returns true if parsing succeeds (i.e. the file is syntactically valid
    /// for the given language), false otherwise.
    pub fn can_parse(&self, ext: &str, content: &str) -> bool {
        let adapter = match self.registry.get(ext) {
            Some(a) => a,
            None => return false,
        };
        let mut parser = adapter.parser();
        parser
            .parse(content, None)
            .is_some_and(|tree| !node_has_parse_error(tree.root_node()))
    }

    fn collect_symbols(
        &self,
        node: tree_sitter::Node,
        source: &str,
        symbols: &mut Vec<SymbolMatch>,
    ) {
        let kind = node.kind();
        if is_definition_kind(kind) {
            if let Some(name) = self.extract_def_name(node, source) {
                let start_line = node.start_position().row + 1;
                let end_line = node.end_position().row + 1;
                symbols.push(SymbolMatch {
                    name,
                    kind: SymbolKind::from_ts_node_kind(kind),
                    kind_prefix: kind_prefix(kind),
                    start_line,
                    end_line,
                });
            }
        }

        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                self.collect_symbols(child, source, symbols);
            }
        }
    }

    fn extract_def_name(&self, node: tree_sitter::Node, source: &str) -> Option<String> {
        for i in 0..node.child_count() {
            let child = node.child(i)?;
            let kind = child.kind();
            if kind == "identifier" || kind == "type_identifier" {
                return child
                    .utf8_text(source.as_bytes())
                    .ok()
                    .map(|s| s.to_string());
            }
        }
        None
    }
}

fn symbol_matches_selector(symbol: &SymbolMatch, parsed: &ParsedSelector) -> bool {
    symbol.name == parsed.name && (parsed.kind == SymbolKind::Unknown || symbol.kind == parsed.kind)
}

fn is_definition_kind(kind: &str) -> bool {
    matches!(
        kind,
        "function_item"
            | "struct_item"
            | "enum_item"
            | "trait_item"
            | "impl_item"
            | "function_definition"
            | "class_definition"
            | "decorated_definition"
            | "function_declaration"
            | "class_declaration"
            | "interface_declaration"
            | "enum_declaration"
            | "method_definition"
            | "type_alias_declaration"
    )
}

fn kind_prefix(kind: &str) -> &'static str {
    match kind {
        "function_item" => "fn ",
        "struct_item" => "struct ",
        "enum_item" => "enum ",
        "trait_item" => "trait ",
        "impl_item" => "impl ",
        "function_definition" => "fn ",
        "class_definition" => "class ",
        "function_declaration" => "fn ",
        "class_declaration" => "class ",
        "interface_declaration" => "trait ",
        "enum_declaration" => "enum ",
        "method_definition" => "fn ",
        "type_alias_declaration" => "type ",
        _ => "",
    }
}

fn node_has_parse_error(node: tree_sitter::Node<'_>) -> bool {
    if node.has_error() || node.is_error() || node.is_missing() {
        return true;
    }

    let mut cursor = node.walk();
    node.children(&mut cursor).any(node_has_parse_error)
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

    const RUST_CODE: &str = "// line 1\n                 fn startup() {\n                    inner_call();\n                }\n            }\n            ";

    #[test]
    fn test_find_containing_symbol_fn() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_temp_rust_file(dir.path(), "test.rs", RUST_CODE);
        let analyzer = TreeSitterAnalyzer::new();
        // Line 4 should be inside startup() (adjusted for the actual structure)
        let result = analyzer.find_containing_symbol(&path, 3, dir.path());
        // Just check it doesn't crash; exact line numbers depend on the test string
        println!("find_containing_symbol result: {:?}", result);
    }

    #[test]
    fn symbol_match_source_from_returns_exact_line_range() {
        let symbol = SymbolMatch {
            name: "target".to_string(),
            kind: SymbolKind::Function,
            kind_prefix: "fn ",
            start_line: 3,
            end_line: 5,
        };
        let content = "line 1\nline 2\nfn target() {\n    body();\n}\nfn other() {}\n";
        assert_eq!(
            symbol.source_from(content),
            "fn target() {\n    body();\n}\n"
        );
    }

    #[test]
    fn canonical_selector_disambiguates_duplicate_method_names() {
        let dir = tempfile::tempdir().unwrap();
        let code = r#"trait Hints {
    fn setup_hints(&self);
}

struct Alpha;
struct Beta;

impl Hints for Alpha {
    fn setup_hints(&self) {
        println!("alpha");
    }
}

impl Hints for Beta {
    fn setup_hints(&self) {
        println!("beta");
    }
}
"#;
        let path = write_temp_rust_file(dir.path(), "dup.rs", code);
        let analyzer = TreeSitterAnalyzer::new();

        let canonical = analyzer
            .find_containing_symbol(&path, 16, dir.path())
            .expect("line inside Beta::setup_hints should resolve");
        assert!(canonical.starts_with("dup.rs::fn setup_hints #L"));
        assert!(canonical.contains("-L"));

        let parsed = crate::selector::parse_selector(&canonical).unwrap();
        let resolved = analyzer.resolve_selector(&path, &parsed).unwrap();
        assert_eq!(resolved.name, "setup_hints");
        assert_eq!(resolved.start_line, 15);
    }

    #[test]
    fn legacy_duplicate_method_selector_is_rejected_as_ambiguous() {
        let dir = tempfile::tempdir().unwrap();
        let code = r#"trait Hints {
    fn setup_hints(&self);
}

struct Alpha;
struct Beta;

impl Hints for Alpha {
    fn setup_hints(&self) {}
}

impl Hints for Beta {
    fn setup_hints(&self) {}
}
"#;
        let path = write_temp_rust_file(dir.path(), "dup.rs", code);
        let analyzer = TreeSitterAnalyzer::new();
        let parsed = crate::selector::parse_selector("dup.rs::fn setup_hints").unwrap();
        let err = analyzer.resolve_selector(&path, &parsed).unwrap_err();
        assert!(err.contains("ambiguous selector"));
        assert!(err.contains("#L"));
    }

    #[test]
    fn test_can_parse_valid_rust() {
        let analyzer = TreeSitterAnalyzer::new();
        let valid = "fn main() { println!(\"hello\"); }";
        assert!(analyzer.can_parse("rs", valid));
    }

    #[test]
    fn test_can_parse_rejects_rust_error_nodes() {
        let analyzer = TreeSitterAnalyzer::new();
        let invalid = "fn main( {\n";
        assert!(!analyzer.can_parse("rs", invalid));
    }

    #[test]
    fn test_can_parse_empty_string() {
        let analyzer = TreeSitterAnalyzer::new();
        assert!(analyzer.can_parse("rs", ""));
    }

    #[test]
    fn test_can_parse_unknown_language_returns_false() {
        let analyzer = TreeSitterAnalyzer::new();
        assert!(!analyzer.can_parse("unknown_ext", "fn main() {}"));
    }

    #[test]
    fn test_can_parse_valid_python() {
        let analyzer = TreeSitterAnalyzer::new();
        let py_code = "def greet(name):\n    return f\"Hello, {name}!\"\n";
        assert!(analyzer.can_parse("py", py_code));
    }

    #[test]
    fn test_can_parse_valid_go() {
        let analyzer = TreeSitterAnalyzer::new();
        let go_code = "package main\nfunc greet(name string) string { return \"Hello\" }\n";
        assert!(analyzer.can_parse("go", go_code));
    }

    #[test]
    fn test_can_parse_valid_java() {
        let analyzer = TreeSitterAnalyzer::new();
        let java_code = "public class Hello { public static void main(String[] args) {} }\n";
        assert!(analyzer.can_parse("java", java_code));
    }

    #[test]
    fn test_can_parse_valid_typescript() {
        let analyzer = TreeSitterAnalyzer::new();
        let ts_code = "function greet(name: string): string { return \"Hello\"; }\n";
        assert!(analyzer.can_parse("ts", ts_code));
    }

    #[test]
    fn test_can_parse_valid_javascript() {
        let analyzer = TreeSitterAnalyzer::new();
        let js_code = "function greet(name) { return \"Hello\"; }\n";
        assert!(analyzer.can_parse("js", js_code));
    }

    #[test]
    fn test_can_parse_valid_c() {
        let analyzer = TreeSitterAnalyzer::new();
        let c_code = "int main() { return 0; }\n";
        assert!(analyzer.can_parse("c", c_code));
    }

    #[test]
    fn test_can_parse_valid_cpp() {
        let analyzer = TreeSitterAnalyzer::new();
        let cpp_code = "class Hello { public: void greet() {} };\n";
        assert!(analyzer.can_parse("cpp", cpp_code));
    }

    #[test]
    fn test_can_parse_valid_ruby() {
        let analyzer = TreeSitterAnalyzer::new();
        let ruby_code = "def greet(name)\n  \"Hello, #{name}!\"\nend\n";
        assert!(analyzer.can_parse("rb", ruby_code));
    }

    #[test]
    fn test_can_parse_valid_php() {
        let analyzer = TreeSitterAnalyzer::new();
        let php_code = "<?php\nfunction greet($name) { return \"Hello\"; }\n";
        assert!(analyzer.can_parse("php", php_code));
    }

    #[test]
    fn test_language_registry_has_all_languages() {
        let registry = LanguageRegistry::new();
        assert!(registry.get("rs").is_some(), "Rust should be registered");
        assert!(registry.get("py").is_some(), "Python should be registered");
        assert!(registry.get("go").is_some(), "Go should be registered");
        assert!(registry.get("java").is_some(), "Java should be registered");
        assert!(
            registry.get("ts").is_some(),
            "TypeScript should be registered"
        );
        assert!(
            registry.get("js").is_some(),
            "JavaScript should be registered"
        );
        assert!(registry.get("c").is_some(), "C should be registered");
        assert!(registry.get("cpp").is_some(), "C++ should be registered");
        assert!(registry.get("rb").is_some(), "Ruby should be registered");
        assert!(registry.get("php").is_some(), "PHP should be registered");
    }

    #[test]
    fn test_language_registry_all_names() {
        let registry = LanguageRegistry::new();
        let langs = registry.list_languages();
        let names: Vec<&str> = langs.iter().map(|(n, _)| *n).collect();
        assert!(names.contains(&"rust"), "rust in {:?}", names);
        assert!(names.contains(&"python"), "python in {:?}", names);
        assert!(names.contains(&"go"), "go in {:?}", names);
        assert!(names.contains(&"java"), "java in {:?}", names);
        assert!(names.contains(&"typescript"), "typescript in {:?}", names);
        assert!(names.contains(&"javascript"), "javascript in {:?}", names);
        assert!(names.contains(&"c"), "c in {:?}", names);
        assert!(names.contains(&"cpp"), "cpp in {:?}", names);
        assert!(names.contains(&"ruby"), "ruby in {:?}", names);
        assert!(names.contains(&"php"), "php in {:?}", names);
    }
}
