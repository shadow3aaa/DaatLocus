use std::path::{Path, PathBuf};

use regex::Regex;

type OptionalLineRangeSuffix<'a> = (&'a str, Option<(usize, usize)>);

/// A parsed SCOPE selector. The selector is a positioning DSL: it locates
/// targets/ranges, but it does not encode operation semantics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSelector {
    /// File path relative to project root.
    pub file_path: PathBuf,
    /// Parsed selector target.
    pub target: SelectorTarget,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectorTarget {
    Symbol(SymbolSelector),
    LineRange {
        start_line: usize,
        end_line: usize,
    },
    AroundLine {
        line: usize,
        context: usize,
    },
    Match {
        pattern: String,
        around: Option<usize>,
    },
    BeforeLine {
        line: usize,
    },
    AfterLine {
        line: usize,
    },
    Enclosing {
        line: usize,
    },
    Outline,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolSelector {
    /// The kind of symbol (function, struct, etc.) or Unknown for bare names.
    pub kind: SymbolKind,
    /// The symbol name to match against AST nodes.
    pub name: String,
    /// Optional 1-based line range disambiguator: `#Lstart-Lend`.
    pub line_range: Option<(usize, usize)>,
}

impl ParsedSelector {
    pub fn as_symbol(&self) -> Option<&SymbolSelector> {
        match &self.target {
            SelectorTarget::Symbol(symbol) => Some(symbol),
            _ => None,
        }
    }

    /// Legacy accessor for callers/tests that still operate on symbol selectors.
    pub fn kind(&self) -> Option<&SymbolKind> {
        self.as_symbol().map(|symbol| &symbol.kind)
    }

    /// Legacy accessor for callers/tests that still operate on symbol selectors.
    pub fn name(&self) -> Option<&str> {
        self.as_symbol().map(|symbol| symbol.name.as_str())
    }

    /// Legacy accessor for callers/tests that still operate on symbol selectors.
    pub fn line_range(&self) -> Option<(usize, usize)> {
        self.as_symbol().and_then(|symbol| symbol.line_range)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SymbolKind {
    Function,
    Struct,
    Enum,
    Trait,
    Impl,
    Class,
    /// Mod, const, static, type alias, or bare name (fuzzy match all).
    Unknown,
}

impl SymbolKind {
    /// Parse a symbol-kind prefix like "fn", "struct", "enum", "trait", "impl".
    fn from_prefix(prefix: &str) -> Self {
        match prefix {
            // Rust
            "fn" => SymbolKind::Function,
            "struct" => SymbolKind::Struct,
            "enum" => SymbolKind::Enum,
            "trait" => SymbolKind::Trait,
            "impl" => SymbolKind::Impl,
            "const" => SymbolKind::Unknown,
            "let" => SymbolKind::Unknown,
            "var" => SymbolKind::Unknown,
            // Go
            "func" => SymbolKind::Function,
            "type" => SymbolKind::Struct,
            "method" => SymbolKind::Function,
            "package" => SymbolKind::Unknown,
            // Java/C++/C#/Ruby/PHP
            "class" => SymbolKind::Class,
            "interface" => SymbolKind::Trait,
            "constructor" => SymbolKind::Function,
            "def" => SymbolKind::Function,
            "module" => SymbolKind::Unknown,
            _ => SymbolKind::Unknown,
        }
    }

    /// Heuristic: guess the kind from a tree-sitter node kind string.
    /// Used to map tree-sitter parse results back to SymbolKind.
    pub fn from_ts_node_kind(kind: &str) -> Self {
        match kind {
            "function_item" => SymbolKind::Function,
            "struct_item" => SymbolKind::Struct,
            "enum_item" => SymbolKind::Enum,
            "trait_item" => SymbolKind::Trait,
            "impl_item" => SymbolKind::Impl,
            // Python tree-sitter node types
            "function_definition" => SymbolKind::Function,
            "class_definition" => SymbolKind::Class,
            "decorated_definition" => SymbolKind::Function,
            // TypeScript/JavaScript node types
            "function_declaration" => SymbolKind::Function,
            "class_declaration" => SymbolKind::Class,
            "interface_declaration" => SymbolKind::Trait,
            "enum_declaration" => SymbolKind::Enum,
            "method_definition" => SymbolKind::Function,
            "arrow_function" => SymbolKind::Function,
            "variable_declarator" => SymbolKind::Function,
            "type_alias_declaration" => SymbolKind::Struct,
            // Go tree-sitter node types
            "method_declaration" => SymbolKind::Function,
            "type_declaration" => SymbolKind::Struct,
            "type_identifier" => SymbolKind::Struct,
            // Java tree-sitter node types
            "constructor_declaration" => SymbolKind::Function,
            "field_declaration" => SymbolKind::Unknown,
            "local_variable_declaration" => SymbolKind::Unknown,
            // C/C++ tree-sitter node types
            "class_specifier" => SymbolKind::Class,
            "struct_specifier" => SymbolKind::Struct,
            // Ruby tree-sitter node types
            "singleton_method" => SymbolKind::Function,
            _ => SymbolKind::Unknown,
        }
    }
}

impl std::fmt::Display for SymbolKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SymbolKind::Function => write!(f, "fn"),
            SymbolKind::Struct => write!(f, "struct"),
            SymbolKind::Enum => write!(f, "enum"),
            SymbolKind::Trait => write!(f, "trait"),
            SymbolKind::Impl => write!(f, "impl"),
            SymbolKind::Class => write!(f, "class"),
            SymbolKind::Unknown => write!(f, "symbol"),
        }
    }
}

/// Parse a selector string of the form `file_path::[kind ]name`.
///
/// The `::` separates the file path from the symbol expression.
/// After `::`, an optional kind prefix (`fn`, `struct`, etc.) may appear,
/// followed by the symbol name.  Trailing `()` is stripped from function names.
///
/// # Errors
/// Returns an error string if the input is missing `::`, has an empty file path,
/// or has an empty symbol name.
pub fn parse_selector(input: &str) -> Result<ParsedSelector, String> {
    if !input.contains("::")
        && let Some((file_part, suffix)) = input.split_once('#')
    {
        return parse_hash_selector(file_part, suffix, input);
    }

    // Split on the first `::`
    let (file_part, symbol_part) = input.split_once("::").ok_or_else(|| {
        format!("selector must contain '::' separating file path from symbol: '{input}'")
    })?;

    let file_path = parse_file_path(file_part)?;

    let symbol_part = symbol_part.trim();
    if symbol_part.is_empty() {
        return Err("selector symbol part is empty after '::'".to_string());
    }

    let (symbol_part, line_range) = parse_line_range_suffix(symbol_part)?;

    // Parse the symbol part: optional kind prefix + name
    let (kind, name) = parse_symbol_expr(symbol_part);

    if name.is_empty() {
        return Err(format!("selector symbol name is empty: '{symbol_part}'"));
    }

    Ok(ParsedSelector {
        file_path,
        target: SelectorTarget::Symbol(SymbolSelector {
            kind,
            name,
            line_range,
        }),
    })
}

fn parse_file_path(file_part: &str) -> Result<PathBuf, String> {
    let file_path = PathBuf::from(file_part.trim());
    if file_path.as_os_str().is_empty() {
        return Err("selector file path is empty".to_string());
    }
    Ok(file_path)
}

fn parse_hash_selector(
    file_part: &str,
    suffix: &str,
    input: &str,
) -> Result<ParsedSelector, String> {
    let file_path = parse_file_path(file_part)?;
    let target = if let Some(range_expr) = suffix.strip_prefix('L') {
        let (start_line, end_line) = parse_line_range_expr(range_expr)?;
        SelectorTarget::LineRange {
            start_line,
            end_line,
        }
    } else if let Some(expr) = suffix.strip_prefix("around:L") {
        let (line, context) = parse_around_line_expr(expr)?;
        SelectorTarget::AroundLine { line, context }
    } else if let Some(expr) = suffix.strip_prefix("match:/") {
        parse_match_target(expr)?
    } else if let Some(expr) = suffix.strip_prefix("before:L") {
        let line = parse_positive_usize(expr, "before line")?;
        SelectorTarget::BeforeLine { line }
    } else if let Some(expr) = suffix.strip_prefix("after:L") {
        let line = parse_positive_usize(expr, "after line")?;
        SelectorTarget::AfterLine { line }
    } else if let Some(expr) = suffix.strip_prefix("enclosing:L") {
        let line = parse_positive_usize(expr, "enclosing line")?;
        SelectorTarget::Enclosing { line }
    } else if suffix == "outline" {
        SelectorTarget::Outline
    } else {
        return Err(format!("unsupported selector suffix in '{input}'"));
    };

    Ok(ParsedSelector { file_path, target })
}

/// Parse an optional `#Lstart-Lend` line-range disambiguator suffix.
fn parse_line_range_suffix(expr: &str) -> Result<OptionalLineRangeSuffix<'_>, String> {
    let Some((symbol_expr, range_expr)) = expr.rsplit_once("#L") else {
        return Ok((expr, None));
    };

    let symbol_expr = symbol_expr.trim_end();
    if symbol_expr.is_empty() {
        return Err(format!(
            "selector symbol name is empty before line range: '{expr}'"
        ));
    }

    let (start_str, end_str) = range_expr
        .split_once("-L")
        .or_else(|| range_expr.split_once('-'))
        .ok_or_else(|| format!("bad selector line range '#L{range_expr}'"))?;
    let start = start_str
        .parse::<usize>()
        .map_err(|_| format!("bad selector line range start: '#L{range_expr}'"))?;
    let end = end_str
        .parse::<usize>()
        .map_err(|_| format!("bad selector line range end: '#L{range_expr}'"))?;
    if start == 0 || end == 0 || start > end {
        return Err(format!("bad selector line range '#L{range_expr}'"));
    }

    Ok((symbol_expr, Some((start, end))))
}

fn parse_line_range_expr(expr: &str) -> Result<(usize, usize), String> {
    let (start_str, end_str) = expr
        .split_once("-L")
        .or_else(|| expr.split_once('-'))
        .ok_or_else(|| format!("bad selector line range '#L{expr}'"))?;
    let start = parse_positive_usize(start_str, "line range start")?;
    let end = parse_positive_usize(end_str, "line range end")?;
    if start > end {
        return Err(format!("bad selector line range '#L{expr}'"));
    }
    Ok((start, end))
}

fn parse_around_line_expr(expr: &str) -> Result<(usize, usize), String> {
    let (line_str, context_str) = expr
        .split_once('±')
        .or_else(|| expr.split_once("+-"))
        .or_else(|| expr.split_once("+/-"))
        .ok_or_else(|| format!("bad around selector '#around:L{expr}'"))?;
    Ok((
        parse_positive_usize(line_str, "around line")?,
        parse_positive_usize(context_str, "around context")?,
    ))
}

fn parse_match_target(expr: &str) -> Result<SelectorTarget, String> {
    let (pattern, rest) = expr
        .split_once('/')
        .ok_or_else(|| "bad match selector; expected #match:/pattern/".to_string())?;
    if pattern.is_empty() {
        return Err("match selector pattern is empty".to_string());
    }
    Regex::new(pattern).map_err(|e| format!("bad match selector regex: {e}"))?;
    let around = if rest.is_empty() {
        None
    } else if let Some(around_expr) = rest.strip_prefix("#around:") {
        Some(parse_positive_usize(around_expr, "match around context")?)
    } else {
        return Err(format!("unsupported match selector suffix: '{rest}'"));
    };
    Ok(SelectorTarget::Match {
        pattern: pattern.to_string(),
        around,
    })
}

fn parse_positive_usize(input: &str, label: &str) -> Result<usize, String> {
    let value = input
        .parse::<usize>()
        .map_err(|_| format!("bad {label}: '{input}'"))?;
    if value == 0 {
        return Err(format!("bad {label}: '{input}'"));
    }
    Ok(value)
}

/// Parse the symbol expression (everything after `::`).
///
/// Recognised forms:
/// - `fn name` or `fn name()` → (Function, "name")
/// - `struct Name`            → (Struct, "Name")
/// - `enum Name`              → (Enum, "Name")
/// - `trait Name`             → (Trait, "Name")
/// - `impl Type`              → (Impl, "Type")
/// - `impl Trait for Type`    → (Impl, "Trait")  — we take the trait name
/// - bare name                → (Unknown, "name")
fn parse_symbol_expr(expr: &str) -> (SymbolKind, String) {
    let expr = expr.trim();

    // Strip trailing `()` from function-like names
    let expr = expr.strip_suffix("()").unwrap_or(expr);

    // Check for kind prefixes
    let parts: Vec<&str> = expr.splitn(2, char::is_whitespace).collect();
    if parts.len() == 2 {
        let prefix = parts[0];
        let remainder = parts[1].trim();
        let kind = SymbolKind::from_prefix(prefix);
        if !matches!(kind, SymbolKind::Unknown) {
            // For `impl Trait for Type`, take the trait name
            if matches!(kind, SymbolKind::Impl)
                && let Some(trait_name) = remainder.split_whitespace().next()
            {
                return (kind, trait_name.to_string());
            }
            // Strip trailing parens from remainder too (e.g. "fn old()")
            let name = remainder.strip_suffix("()").unwrap_or(remainder);
            return (kind, name.to_string());
        }
    }

    // Bare name — fuzzy match all kinds
    (SymbolKind::Unknown, expr.to_string())
}

/// Resolve a selector's file path against a project root, and return the absolute path
/// along with the file extension (for language detection).
pub fn resolve_file(
    selector: &ParsedSelector,
    project_root: &Path,
) -> Result<(PathBuf, String), String> {
    let full_path = project_root.join(&selector.file_path);
    if !full_path.exists() {
        return Err(format!("file not found: {}", full_path.display()));
    }
    let ext = full_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_string();
    if ext.is_empty() {
        return Err(format!(
            "cannot determine language from file: {}",
            full_path.display()
        ));
    }
    Ok((full_path, ext))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_fn_selector() {
        let sel = parse_selector("src/foo.rs::fn authenticate").unwrap();
        assert_eq!(sel.file_path, PathBuf::from("src/foo.rs"));
        assert_eq!(sel.kind(), Some(&SymbolKind::Function));
        assert_eq!(sel.name(), Some("authenticate"));
    }

    #[test]
    fn parse_fn_with_parens() {
        let sel = parse_selector("src/foo.rs::fn authenticate()").unwrap();
        assert_eq!(sel.name(), Some("authenticate"));
    }

    #[test]
    fn parse_line_range_disambiguator() {
        let sel = parse_selector("src/foo.rs::fn authenticate #L10-L20").unwrap();
        assert_eq!(sel.name(), Some("authenticate"));
        assert_eq!(sel.line_range(), Some((10, 20)));
    }

    #[test]
    fn invalid_line_range_disambiguator_is_error() {
        assert!(parse_selector("src/foo.rs::fn authenticate #L20-L10").is_err());
        assert!(parse_selector("src/foo.rs::fn authenticate #Labc-L20").is_err());
    }

    #[test]
    fn parse_struct_selector() {
        let sel = parse_selector("src/lib.rs::struct Config").unwrap();
        assert_eq!(sel.kind(), Some(&SymbolKind::Struct));
        assert_eq!(sel.name(), Some("Config"));
    }

    #[test]
    fn parse_enum_selector() {
        let sel = parse_selector("src/types.rs::enum Color").unwrap();
        assert_eq!(sel.kind(), Some(&SymbolKind::Enum));
        assert_eq!(sel.name(), Some("Color"));
    }

    #[test]
    fn parse_trait_selector() {
        let sel = parse_selector("src/lib.rs::trait Serialize").unwrap();
        assert_eq!(sel.kind(), Some(&SymbolKind::Trait));
        assert_eq!(sel.name(), Some("Serialize"));
    }

    #[test]
    fn parse_impl_selector() {
        let sel = parse_selector("src/foo.rs::impl MyStruct").unwrap();
        assert_eq!(sel.kind(), Some(&SymbolKind::Impl));
        assert_eq!(sel.name(), Some("MyStruct"));
    }

    #[test]
    fn parse_impl_for_selector() {
        let sel = parse_selector("src/foo.rs::impl Display for MyStruct").unwrap();
        assert_eq!(sel.kind(), Some(&SymbolKind::Impl));
        assert_eq!(sel.name(), Some("Display"));
    }

    #[test]
    fn parse_bare_name() {
        let sel = parse_selector("src/foo.rs::authenticate").unwrap();
        assert_eq!(sel.kind(), Some(&SymbolKind::Unknown));
        assert_eq!(sel.name(), Some("authenticate"));
    }

    #[test]
    fn parse_with_spaces_in_path() {
        // File paths with spaces are unusual but valid
        let sel = parse_selector("some dir/file.rs::fn hello").unwrap();
        assert_eq!(sel.file_path, PathBuf::from("some dir/file.rs"));
        assert_eq!(sel.name(), Some("hello"));
    }

    #[test]
    fn missing_double_colon_is_error() {
        assert!(parse_selector("src/foo.rs").is_err());
    }

    #[test]
    fn empty_file_path_is_error() {
        assert!(parse_selector("::fn foo").is_err());
    }

    #[test]
    fn empty_symbol_is_error() {
        assert!(parse_selector("src/foo.rs::").is_err());
    }
    #[test]
    fn parse_scope_range_and_context_selectors() {
        let sel = parse_selector("src/foo.rs#L120-L180").unwrap();
        assert_eq!(sel.file_path, PathBuf::from("src/foo.rs"));
        assert_eq!(
            sel.target,
            SelectorTarget::LineRange {
                start_line: 120,
                end_line: 180
            }
        );

        let sel = parse_selector("src/foo.rs#around:L150±40").unwrap();
        assert_eq!(
            sel.target,
            SelectorTarget::AroundLine {
                line: 150,
                context: 40
            }
        );

        let sel = parse_selector("src/foo.rs#enclosing:L150").unwrap();
        assert_eq!(sel.target, SelectorTarget::Enclosing { line: 150 });

        let sel = parse_selector("src/foo.rs#before:L150").unwrap();
        assert_eq!(sel.target, SelectorTarget::BeforeLine { line: 150 });

        let sel = parse_selector("src/foo.rs#after:L150").unwrap();
        assert_eq!(sel.target, SelectorTarget::AfterLine { line: 150 });

        let sel = parse_selector("src/foo.rs#outline").unwrap();
        assert_eq!(sel.target, SelectorTarget::Outline);
    }

    #[test]
    fn parse_scope_match_selectors() {
        let sel = parse_selector("src/foo.rs#match:/ProjectInstructions/").unwrap();
        assert_eq!(
            sel.target,
            SelectorTarget::Match {
                pattern: "ProjectInstructions".to_string(),
                around: None
            }
        );

        let sel = parse_selector("src/foo.rs#match:/ProjectInstructions/#around:40").unwrap();
        assert_eq!(
            sel.target,
            SelectorTarget::Match {
                pattern: "ProjectInstructions".to_string(),
                around: Some(40)
            }
        );
    }
}
