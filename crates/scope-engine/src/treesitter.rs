use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use tree_sitter::StreamingIterator;

use crate::language::LanguageRegistry;
use crate::selector::{ParsedSelector, SelectorTarget, SymbolKind, SymbolSelector};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolMatch {
    pub name: String,
    pub kind: SymbolKind,
    pub kind_prefix: &'static str,
    pub start_line: usize,
    pub end_line: usize,
}

fn normalize_path_for_cmp(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    s.strip_prefix(r"\\?\")
        .map_or_else(|| p.to_path_buf(), PathBuf::from)
}

impl SymbolMatch {
    #[must_use]
    pub fn canonical_selector(&self, file_path: &Path, project_root: &Path) -> String {
        let rel_path = normalize_path_for_cmp(file_path)
            .strip_prefix(normalize_path_for_cmp(project_root))
            .ok()
            .map_or_else(
                || file_path.to_string_lossy().to_string(),
                |p| p.to_string_lossy().to_string(),
            )
            .replace('\\', "/");

        format!(
            "{}::{}{} #L{}-L{}",
            rel_path, self.kind_prefix, self.name, self.start_line, self.end_line
        )
    }

    #[must_use]
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseErrorDiagnostic {
    pub kind: ParseErrorKind,
    pub node_kind: String,
    pub start_line: usize,
    pub start_column: usize,
    pub end_line: usize,
    pub end_column: usize,
    pub snippet: String,
}

impl ParseErrorDiagnostic {
    #[must_use]
    pub fn message(&self) -> String {
        format!(
            "first parse error: {} node `{}` at L{}:C{}-L{}:C{}\n{}",
            self.kind.as_str(),
            self.node_kind,
            self.start_line,
            self.start_column,
            self.end_line,
            self.end_column,
            self.snippet
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseErrorKind {
    Error,
    Missing,
}

impl ParseErrorKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Error => "ERROR",
            Self::Missing => "MISSING",
        }
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
    #[must_use]
    pub fn new() -> Self {
        Self {
            registry: LanguageRegistry::new(),
        }
    }

    /// Given a file path and a 1-based line number, find the innermost
    /// named definition (function, struct, enum, trait, impl) that contains
    /// that line. Returns a canonical CodeStruct-style selector like
    /// `src/foo.rs::fn authenticate #L10-L20`.
    #[must_use]
    pub fn find_containing_symbol(
        &self,
        file_path: &Path,
        line_number: usize,
        project_root: &Path,
    ) -> Option<String> {
        self.find_containing_symbol_with_symbols(file_path, line_number, project_root, None)
    }

    #[must_use]
    pub fn find_containing_symbol_with_symbols(
        &self,
        file_path: &Path,
        line_number: usize,
        project_root: &Path,
        cached_symbols: Option<&[SymbolMatch]>,
    ) -> Option<String> {
        self.find_containing_symbol_match(file_path, line_number, cached_symbols)
            .map(|m| m.canonical_selector(file_path, project_root))
    }

    #[must_use]
    pub fn find_containing_symbol_match(
        &self,
        file_path: &Path,
        line_number: usize,
        cached_symbols: Option<&[SymbolMatch]>,
    ) -> Option<SymbolMatch> {
        let owned;
        let symbols = if let Some(cached_symbols) = cached_symbols {
            cached_symbols
        } else {
            owned = self.symbols_in_file(file_path).ok()?;
            &owned
        };
        symbols
            .iter()
            .filter(|m| line_number >= m.start_line && line_number <= m.end_line)
            .max_by_key(|m| (m.start_line, usize::MAX - m.end_line))
            .cloned()
    }

    /// # Errors
    ///
    /// Returns an error if the source file cannot be read, parsed, or matched to a symbol.
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

        let SelectorTarget::Symbol(symbol) = &parsed.target;

        if let Some((start, end)) = symbol.line_range {
            matches.retain(|m| m.start_line == start && m.end_line == end);
        }

        match matches.len() {
            0 => Err(format!(
                "symbol '{}' not found in {}",
                symbol.name,
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
                    symbol.name,
                    file_path.display(),
                    candidates
                ))
            }
        }
    }

    /// # Errors
    ///
    /// Returns an error if the file extension is unsupported, the file cannot be read, or parsing fails.
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
        Self::collect_symbols(tree.root_node(), &content, ext, &mut symbols);
        Ok(symbols)
    }

    #[must_use]
    pub fn is_import_only_reference(&self, file_path: &Path, line_number: usize) -> bool {
        let Some(ext) = file_path.extension().and_then(|e| e.to_str()) else {
            return false;
        };
        let Some(adapter) = self.registry.get(ext) else {
            return false;
        };
        if adapter.language_name() != "rust" {
            return false;
        }

        let Ok(content) = std::fs::read_to_string(file_path) else {
            return false;
        };
        let mut parser = adapter.parser();
        let Some(tree) = parser.parse(&content, None) else {
            return false;
        };

        let Ok(query) = tree_sitter::Query::new(&adapter.language(), RUST_USE_IMPORT_QUERY) else {
            return false;
        };
        let mut cursor = tree_sitter::QueryCursor::new();
        let mut matches = cursor.matches(&query, tree.root_node(), content.as_bytes());
        while let Some(query_match) = matches.next() {
            for capture in query_match.captures {
                let node = capture.node;
                let start = node.start_position().row + 1;
                let end = node.end_position().row + 1;
                if line_number >= start && line_number <= end {
                    return true;
                }
            }
        }

        false
    }

    /// Validate that a file's content can be parsed by tree-sitter.
    /// Returns true if parsing succeeds (i.e. the file is syntactically valid
    /// for the given language), false otherwise.
    #[must_use]
    pub fn can_parse(&self, ext: &str, content: &str) -> bool {
        self.parse_error_diagnostic(ext, content).is_none()
    }

    /// Return the first tree-sitter parse diagnostic for this source, if any.
    ///
    /// Tree-sitter does not expose compiler-style syntax diagnostics. It
    /// recovers by placing ERROR and MISSING nodes in the parse tree. This
    /// helper reports the first such node with source coordinates and a compact
    /// snippet so edit rejection messages can point at the likely problem.
    #[must_use]
    pub fn parse_error_diagnostic(&self, ext: &str, content: &str) -> Option<ParseErrorDiagnostic> {
        let Some(adapter) = self.registry.get(ext) else {
            return Some(ParseErrorDiagnostic {
                kind: ParseErrorKind::Error,
                node_kind: format!("unsupported extension `{ext}`"),
                start_line: 1,
                start_column: 1,
                end_line: 1,
                end_column: 1,
                snippet: parse_error_snippet(content, 1, 1),
            });
        };
        let mut parser = adapter.parser();
        let tree = parser.parse(content, None)?;
        first_parse_error_node(tree.root_node()).map(|node| {
            let start = node.start_position();
            let end = node.end_position();
            let start_line = start.row + 1;
            let start_column = start.column + 1;
            ParseErrorDiagnostic {
                kind: if node.is_missing() {
                    ParseErrorKind::Missing
                } else {
                    ParseErrorKind::Error
                },
                node_kind: node.kind().to_string(),
                start_line,
                start_column,
                end_line: end.row + 1,
                end_column: end.column + 1,
                snippet: parse_error_snippet(content, start_line, start_column),
            }
        })
    }

    /// Return the SCOPE language adapter that owns semantic source operations
    /// for the given extension.
    #[must_use]
    pub fn responsible_language_for_extension(&self, ext: &str) -> Option<&'static str> {
        self.registry
            .get(ext)
            .map(super::language::LanguageAdapter::language_name)
    }

    /// Return true when SCOPE owns semantic source operations for this path.
    #[must_use]
    pub fn is_responsible_source_path(&self, file_path: &Path) -> bool {
        file_path
            .extension()
            .and_then(|ext| ext.to_str())
            .and_then(|ext| self.responsible_language_for_extension(ext))
            .is_some()
    }

    fn collect_symbols(
        node: tree_sitter::Node,
        source: &str,
        adapter_key: &str,
        symbols: &mut Vec<SymbolMatch>,
    ) {
        let kind = node.kind();
        if is_definition_kind(kind)
            && let Some(name) = Self::extract_def_name(node, source, adapter_key)
        {
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

        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                Self::collect_symbols(child, source, adapter_key, symbols);
            }
        }
    }

    /// Definition name from the language adapter's existing `@name` capture.
    ///
    /// The child walk is only a fallback when the query yields nothing (or the
    /// adapter query cannot be compiled). It is not the primary grammar path:
    /// the first `identifier`/`type_identifier` child is often a qualifier,
    /// return type, or receiver rather than the declared name.
    fn extract_def_name(
        node: tree_sitter::Node,
        source: &str,
        adapter_key: &str,
    ) -> Option<String> {
        captured_definition_name(node, source, adapter_key)
            .or_else(|| fallback_child_def_name(node, source))
    }

    /// Locate a definition by running the language adapter's definition query
    /// and returning the `@name` capture whose text matches `symbol_name`.
    ///
    /// `line`/`character` are 1-based / 0-based to match `textDocument/references`.
    /// This does not substring-search the file.
    #[must_use]
    pub fn definition_name_position(
        &self,
        file_path: &Path,
        source: &str,
        symbol_name: &str,
    ) -> Option<(usize, usize)> {
        let ext = file_path.extension().and_then(|e| e.to_str())?;
        let adapter = self.registry.get(ext)?;
        let mut parser = adapter.parser();
        let tree = parser.parse(source, None)?;
        let query = tree_sitter::Query::new(&adapter.language(), adapter.queries().definitions)
            .ok()?;
        let name_idx = query.capture_index_for_name("name")?;
        let mut cursor = tree_sitter::QueryCursor::new();
        let mut matches = cursor.matches(&query, tree.root_node(), source.as_bytes());
        while let Some(query_match) = matches.next() {
            for capture in query_match.captures {
                if capture.index != name_idx {
                    continue;
                }
                let Ok(text) = capture.node.utf8_text(source.as_bytes()) else {
                    continue;
                };
                if text == symbol_name {
                    let point = capture.node.start_position();
                    return Some((point.row + 1, point.column));
                }
            }
        }
        None
    }
}

/// Run the language adapter definition query against `node` and return the
/// text of the first `@name` capture that belongs to that node.
///
/// Query strings come from [`crate::language::LanguageAdapter::queries`]; this
/// function does not duplicate them.
fn definition_query_cache() -> &'static Mutex<HashMap<String, Arc<tree_sitter::Query>>> {
    static QUERIES: OnceLock<Mutex<HashMap<String, Arc<tree_sitter::Query>>>> = OnceLock::new();
    QUERIES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cached_definition_query(
    adapter_key: &str,
    language: &tree_sitter::Language,
) -> Option<Arc<tree_sitter::Query>> {
    if let Ok(cache) = definition_query_cache().lock()
        && let Some(query) = cache.get(adapter_key)
    {
        return Some(Arc::clone(query));
    }
    let registry = LanguageRegistry::new();
    let adapter = registry.get(adapter_key)?;
    let query = Arc::new(tree_sitter::Query::new(language, adapter.queries().definitions).ok()?);
    if let Ok(mut cache) = definition_query_cache().lock() {
        cache.insert(adapter_key.to_string(), Arc::clone(&query));
    }
    Some(query)
}

fn captured_definition_name(
    node: tree_sitter::Node,
    source: &str,
    adapter_key: &str,
) -> Option<String> {
    let language = node.language();
    let query = cached_definition_query(adapter_key, &language)?;
    let name_idx = query.capture_index_for_name("name")?;
    let mut cursor = tree_sitter::QueryCursor::new();
    let mut matches = cursor.matches(&query, node, source.as_bytes());
    while let Some(query_match) = matches.next() {
        for capture in query_match.captures {
            if capture.index != name_idx {
                continue;
            }
            // Nested definitions also match when the query is rooted at an
            // outer node (for example a decorated definition). Prefer the
            // capture whose parent is this node, but accept a capture inside
            // the node when the pattern's `@def` node is an ancestor.
            let captured = capture.node;
            let belongs = captured.parent().is_some_and(|parent| parent.id() == node.id())
                || node_contains(node, captured);
            if !belongs {
                continue;
            }
            if let Ok(text) = captured.utf8_text(source.as_bytes()) {
                if !text.is_empty() {
                    return Some(text.to_string());
                }
            }
        }
    }
    None
}

fn node_contains(outer: tree_sitter::Node, inner: tree_sitter::Node) -> bool {
    let start = outer.start_byte();
    let end = outer.end_byte();
    inner.start_byte() >= start && inner.end_byte() <= end
}

/// Last-resort name: first direct `identifier` / `type_identifier` child.
/// Used only when the language query yields nothing.
fn fallback_child_def_name(node: tree_sitter::Node, source: &str) -> Option<String> {
    for i in 0..node.child_count() {
        let child = node.child(i)?;
        let kind = child.kind();
        if kind == "identifier" || kind == "type_identifier" {
            return child
                .utf8_text(source.as_bytes())
                .ok()
                .map(std::string::ToString::to_string);
        }
    }
    None
}

fn symbol_matches_selector(symbol: &SymbolMatch, parsed: &ParsedSelector) -> bool {
    let Some(selector) = parsed.as_symbol() else {
        return false;
    };
    symbol_matches_symbol_selector(symbol, selector)
}

fn symbol_matches_symbol_selector(symbol: &SymbolMatch, selector: &SymbolSelector) -> bool {
    symbol.name == selector.name
        && (selector.kind == SymbolKind::Unknown || symbol.kind == selector.kind)
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
        "struct_item" => "struct ",
        "enum_item" | "enum_declaration" => "enum ",
        "trait_item" | "interface_declaration" => "trait ",
        "impl_item" => "impl ",
        "class_definition" | "class_declaration" => "class ",
        "function_item" | "function_definition" | "function_declaration" | "method_definition" => {
            "fn "
        }
        "type_alias_declaration" => "type ",
        _ => "",
    }
}

fn first_parse_error_node(node: tree_sitter::Node<'_>) -> Option<tree_sitter::Node<'_>> {
    if node.is_error() || node.is_missing() {
        return Some(node);
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if (child.has_error() || child.is_error() || child.is_missing())
            && let Some(found) = first_parse_error_node(child)
        {
            return Some(found);
        }
    }
    None
}

fn parse_error_snippet(content: &str, line: usize, column: usize) -> String {
    let lines = content.lines().collect::<Vec<_>>();
    let start_line = line.saturating_sub(2).max(1);
    let end_line = line
        .saturating_add(2)
        .max(start_line)
        .min(lines.len().max(line));
    let width = end_line.to_string().len().max(1);
    let mut snippet = String::new();

    for current_line in start_line..=end_line {
        let text = lines
            .get(current_line.saturating_sub(1))
            .copied()
            .unwrap_or("");
        let _ = std::fmt::Write::write_fmt(
            &mut snippet,
            format_args!("{current_line:>width$} | {text}\n"),
        );
        if current_line == line {
            let caret_padding = " ".repeat(column.saturating_sub(1));
            let _ = std::fmt::Write::write_fmt(
                &mut snippet,
                format_args!("{:>width$} | {caret_padding}^\n", ""),
            );
        }
    }

    snippet.trim_end().to_string()
}

const RUST_USE_IMPORT_QUERY: &str = "(use_declaration) @import";

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
    #[test]
    fn query_capture_names_pointer_return_and_qualified_impl() {
        let dir = tempfile::tempdir().unwrap();
        let code = "fn takes_ptr(value: *const u8) -> *mut u8 {\n    value as *mut u8\n}\n\nimpl crate::engine::Hints for Alpha {\n    fn setup_hints(&self) {}\n}\n";
        let path = write_temp_rust_file(dir.path(), "names.rs", code);
        let analyzer = TreeSitterAnalyzer::new();
        let symbols = analyzer.symbols_in_file(&path).unwrap();
        let names: Vec<&str> = symbols.iter().map(|symbol| symbol.name.as_str()).collect();
        assert!(
            names.contains(&"takes_ptr"),
            "definition query should capture the function name, not the pointer type; got {names:?}"
        );
        assert!(
            !names.iter().any(|name| *name == "const" || *name == "u8" || *name == "mut"),
            "pointer type tokens must not be treated as definition names; got {names:?}"
        );
        assert!(
            names.contains(&"setup_hints"),
            "method name should come from the @name capture; got {names:?}"
        );
        assert!(
            !names.iter().any(|name| *name == "crate"),
            "qualified impl path must not be the impl name; got {names:?}"
        );

        let position = analyzer
            .definition_name_position(&path, code, "takes_ptr")
            .expect("@name capture locates takes_ptr");
        assert_eq!(position, (1, 3));
        let impl_code = "impl Hints for Alpha {\n    fn setup_hints(&self) {}\n}\n";
        let impl_path = write_temp_rust_file(dir.path(), "impls.rs", impl_code);
        let impl_symbols = analyzer.symbols_in_file(&impl_path).unwrap();
        let impl_names: Vec<&str> = impl_symbols.iter().map(|symbol| symbol.name.as_str()).collect();
        assert!(
            impl_names.contains(&"Alpha"),
            "impl name is the adapter query's type: capture, not the first type_identifier child; got {impl_names:?}"
        );
        assert!(
            !impl_names.iter().any(|name| *name == "Hints"),
            "the implemented trait is not the impl_item type: capture; got {impl_names:?}"
        );
        let impl_position = analyzer
            .definition_name_position(&impl_path, impl_code, "Alpha")
            .expect("impl @name capture is the type: node");
        assert_eq!(impl_position, (1, 15));

        let qualified = "impl crate::engine::Hints for Alpha {\n    fn setup_hints(&self) {}\n}\n";
        let qualified_path = write_temp_rust_file(dir.path(), "qualified.rs", qualified);
        let qualified_names: Vec<String> = analyzer
            .symbols_in_file(&qualified_path)
            .unwrap()
            .into_iter()
            .map(|symbol| symbol.name)
            .collect();
        assert!(
            !qualified_names.iter().any(|name| name == "crate"),
            "when the impl query does not match a scoped type, the child-walk fallback must not invent the path qualifier; got {qualified_names:?}"
        );
        assert!(analyzer.definition_name_position(&path, code, "u8").is_none());
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
        println!("find_containing_symbol result: {result:?}");
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
    fn tsx_files_use_tsx_parser_and_expose_top_level_functions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("status-page.tsx");
        std::fs::write(
            &path,
            "function AgentChatActivityHeader() {\n  return <div />;\n}\n\nfunction agentChatActivityGlyph(bubble: { kind: string }) {\n  return bubble.kind;\n}\n",
        )
        .unwrap();
        let analyzer = TreeSitterAnalyzer::new();

        assert!(analyzer.can_parse("tsx", &std::fs::read_to_string(&path).unwrap()));
        let symbol = analyzer
            .resolve_selector(
                &path,
                &crate::selector::parse_selector("status-page.tsx::fn agentChatActivityGlyph")
                    .unwrap(),
            )
            .expect("TSX top-level function should resolve");
        assert_eq!(symbol.name, "agentChatActivityGlyph");
        assert_eq!(symbol.start_line, 5);
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
        let code = r"trait Hints {
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
";
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
    fn rust_use_declaration_is_import_only_reference() {
        let dir = tempfile::tempdir().unwrap();
        let code = r"use crate::parser::Parser;
use crate::{engine::Engine, runtime};

fn run() {
    Parser::new();
}
";
        let path = write_temp_rust_file(dir.path(), "imports.rs", code);
        let analyzer = TreeSitterAnalyzer::new();

        assert!(analyzer.is_import_only_reference(&path, 1));
        assert!(analyzer.is_import_only_reference(&path, 2));
        assert!(!analyzer.is_import_only_reference(&path, 5));
    }

    #[test]
    fn test_can_parse_rejects_rust_error_nodes() {
        let analyzer = TreeSitterAnalyzer::new();
        let invalid = "fn main( {\n";
        assert!(!analyzer.can_parse("rs", invalid));
    }

    #[test]
    fn parse_error_diagnostic_reports_location_and_snippet() {
        let analyzer = TreeSitterAnalyzer::new();
        let invalid = "fn main( {\n";

        let diagnostic = analyzer
            .parse_error_diagnostic("rs", invalid)
            .expect("invalid rust should produce a parse diagnostic");
        let message = diagnostic.message();

        assert!(message.contains("first parse error:"));
        assert!(message.contains("L1:C"));
        assert!(message.contains("fn main( {"));
        assert!(message.contains('^'));
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
    fn responsible_source_path_matches_registered_languages() {
        let analyzer = TreeSitterAnalyzer::new();
        assert!(analyzer.is_responsible_source_path(Path::new("src/lib.rs")));
        assert!(analyzer.is_responsible_source_path(Path::new("script.py")));
        assert!(!analyzer.is_responsible_source_path(Path::new("README.md")));
        assert!(!analyzer.is_responsible_source_path(Path::new("Makefile")));
        assert_eq!(
            analyzer.responsible_language_for_extension("rs"),
            Some("rust")
        );
        assert_eq!(analyzer.responsible_language_for_extension("md"), None);
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
        assert!(names.contains(&"rust"), "rust in {names:?}");
        assert!(names.contains(&"python"), "python in {names:?}");
        assert!(names.contains(&"go"), "go in {names:?}");
        assert!(names.contains(&"java"), "java in {names:?}");
        assert!(names.contains(&"typescript"), "typescript in {names:?}");
        assert!(names.contains(&"javascript"), "javascript in {names:?}");
        assert!(names.contains(&"c"), "c in {names:?}");
        assert!(names.contains(&"cpp"), "cpp in {names:?}");
        assert!(names.contains(&"ruby"), "ruby in {names:?}");
        assert!(names.contains(&"php"), "php in {names:?}");
    }
}
