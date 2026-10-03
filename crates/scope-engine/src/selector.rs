use std::path::PathBuf;

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
    #[must_use]
    pub const fn as_symbol(&self) -> Option<&SymbolSelector> {
        match &self.target {
            SelectorTarget::Symbol(symbol) => Some(symbol),
        }
    }

    /// Symbol name accessor. Still used by patch planning.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.as_symbol().map(|symbol| symbol.name.as_str())
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
            "struct" | "type" => Self::Struct,
            "enum" => Self::Enum,
            "trait" | "interface" => Self::Trait,
            "impl" => Self::Impl,
            // Go
            // Java/C++/C#/Ruby/PHP
            "class" => Self::Class,
            "fn" | "func" | "method" | "constructor" | "def" => Self::Function,
            _ => Self::Unknown,
        }
    }

    /// Heuristic: guess the kind from a tree-sitter node kind string.
    /// Used to map tree-sitter parse results back to `SymbolKind`.
    #[must_use]
    pub fn from_ts_node_kind(kind: &str) -> Self {
        match kind {
            "struct_item"
            | "type_alias_declaration"
            | "type_declaration"
            | "type_identifier"
            | "struct_specifier" => Self::Struct,
            "enum_item" | "enum_declaration" => Self::Enum,
            "trait_item" | "interface_declaration" => Self::Trait,
            "impl_item" => Self::Impl,
            // Python tree-sitter node types
            "class_definition" | "class_declaration" | "class_specifier" => Self::Class,
            // TypeScript/JavaScript node types
            // Go tree-sitter node types
            // Java tree-sitter node types
            // C/C++ tree-sitter node types
            // Ruby tree-sitter node types
            "function_item"
            | "function_definition"
            | "decorated_definition"
            | "function_declaration"
            | "method_definition"
            | "arrow_function"
            | "variable_declarator"
            | "method_declaration"
            | "constructor_declaration"
            | "singleton_method" => Self::Function,
            _ => Self::Unknown,
        }
    }
}

impl std::fmt::Display for SymbolKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Function => write!(f, "fn"),
            Self::Struct => write!(f, "struct"),
            Self::Enum => write!(f, "enum"),
            Self::Trait => write!(f, "trait"),
            Self::Impl => write!(f, "impl"),
            Self::Class => write!(f, "class"),
            Self::Unknown => write!(f, "symbol"),
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
fn parse_line_range_suffix(symbol_part: &str) -> Result<OptionalLineRangeSuffix<'_>, String> {
    let Some((head, suffix)) = symbol_part.rsplit_once("#L") else {
        return Ok((symbol_part, None));
    };
    let Some((start_text, end_text)) = suffix.split_once("-L") else {
        return Err(format!("invalid line range disambiguator: '{symbol_part}'"));
    };
    let Ok(start) = start_text.trim().parse::<usize>() else {
        return Err(format!("invalid line range disambiguator: '{symbol_part}'"));
    };
    let Ok(end) = end_text.trim().parse::<usize>() else {
        return Err(format!("invalid line range disambiguator: '{symbol_part}'"));
    };
    if start == 0 || end == 0 || start > end {
        return Err(format!("invalid line range disambiguator: '{symbol_part}'"));
    }
    Ok((head.trim_end(), Some((start, end))))
}

fn parse_file_path(file_part: &str) -> Result<PathBuf, String> {
    let file_path = PathBuf::from(file_part.trim());
    if file_path.as_os_str().is_empty() {
        return Err("selector file path is empty".to_string());
    }
    Ok(file_path)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_fn_selector() {
        let sel = parse_selector("src/foo.rs::fn authenticate").unwrap();
        assert_eq!(sel.file_path, PathBuf::from("src/foo.rs"));
        assert_eq!(
            sel.as_symbol().map(|symbol| &symbol.kind),
            Some(&SymbolKind::Function)
        );
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
        assert_eq!(
            sel.as_symbol().and_then(|symbol| symbol.line_range),
            Some((10, 20))
        );
    }

    #[test]
    fn invalid_line_range_disambiguator_is_error() {
        assert!(parse_selector("src/foo.rs::fn authenticate #L20-L10").is_err());
        assert!(parse_selector("src/foo.rs::fn authenticate #Labc-L20").is_err());
    }

    #[test]
    fn parse_struct_selector() {
        let sel = parse_selector("src/lib.rs::struct Config").unwrap();
        assert_eq!(
            sel.as_symbol().map(|symbol| &symbol.kind),
            Some(&SymbolKind::Struct)
        );
        assert_eq!(sel.name(), Some("Config"));
    }

    #[test]
    fn parse_enum_selector() {
        let sel = parse_selector("src/types.rs::enum Color").unwrap();
        assert_eq!(
            sel.as_symbol().map(|symbol| &symbol.kind),
            Some(&SymbolKind::Enum)
        );
        assert_eq!(sel.name(), Some("Color"));
    }

    #[test]
    fn parse_trait_selector() {
        let sel = parse_selector("src/lib.rs::trait Serialize").unwrap();
        assert_eq!(
            sel.as_symbol().map(|symbol| &symbol.kind),
            Some(&SymbolKind::Trait)
        );
        assert_eq!(sel.name(), Some("Serialize"));
    }

    #[test]
    fn parse_impl_selector() {
        let sel = parse_selector("src/foo.rs::impl MyStruct").unwrap();
        assert_eq!(
            sel.as_symbol().map(|symbol| &symbol.kind),
            Some(&SymbolKind::Impl)
        );
        assert_eq!(sel.name(), Some("MyStruct"));
    }

    #[test]
    fn parse_impl_for_selector() {
        let sel = parse_selector("src/foo.rs::impl Display for MyStruct").unwrap();
        assert_eq!(
            sel.as_symbol().map(|symbol| &symbol.kind),
            Some(&SymbolKind::Impl)
        );
        assert_eq!(sel.name(), Some("Display"));
    }

    #[test]
    fn parse_bare_name() {
        let sel = parse_selector("src/foo.rs::authenticate").unwrap();
        assert_eq!(
            sel.as_symbol().map(|symbol| &symbol.kind),
            Some(&SymbolKind::Unknown)
        );
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
}
