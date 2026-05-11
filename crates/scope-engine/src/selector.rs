use std::path::{Path, PathBuf};

/// A parsed CodeStruct-style selector: `file_path::kind symbol`
///
/// Examples:
/// - `src/foo.rs::fn old()`  → file="src/foo.rs", kind=Function, name="old"
/// - `src/lib.rs::struct Foo` → file="src/lib.rs", kind=Struct, name="Foo"
/// - `src/bar.rs::auth`      → file="src/bar.rs", kind=Unknown, name="auth"
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSelector {
    /// File path relative to project root.
    pub file_path: PathBuf,
    /// The kind of symbol (function, struct, etc.) or Unknown for bare names.
    pub kind: SymbolKind,
    /// The symbol name to match against AST nodes.
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SymbolKind {
    Function,
    Struct,
    Enum,
    Trait,
    Impl,
    /// Mod, const, static, type alias, or bare name (fuzzy match all).
    Unknown,
}

impl SymbolKind {
    /// Parse a symbol-kind prefix like "fn", "struct", "enum", "trait", "impl".
    fn from_prefix(prefix: &str) -> Self {
        match prefix {
            "fn" => SymbolKind::Function,
            "struct" => SymbolKind::Struct,
            "enum" => SymbolKind::Enum,
            "trait" => SymbolKind::Trait,
            "impl" => SymbolKind::Impl,
            _ => SymbolKind::Unknown,
        }
    }

    /// Heuristic: guess the kind from a tree-sitter node kind string.
    pub fn from_ts_node_kind(kind: &str) -> Self {
        match kind {
            "function_item" => SymbolKind::Function,
            "struct_item" => SymbolKind::Struct,
            "enum_item" => SymbolKind::Enum,
            "trait_item" => SymbolKind::Trait,
            "impl_item" => SymbolKind::Impl,
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
    // Split on the first `::`
    let (file_part, symbol_part) = input
        .split_once("::")
        .ok_or_else(|| format!("selector must contain '::' separating file path from symbol: '{input}'"))?;

    let file_path = PathBuf::from(file_part.trim());
    if file_path.as_os_str().is_empty() {
        return Err("selector file path is empty".to_string());
    }

    let symbol_part = symbol_part.trim();
    if symbol_part.is_empty() {
        return Err("selector symbol part is empty after '::'".to_string());
    }

    // Parse the symbol part: optional kind prefix + name
    let (kind, name) = parse_symbol_expr(symbol_part);

    if name.is_empty() {
        return Err(format!("selector symbol name is empty: '{symbol_part}'"));
    }

    // Normalise: if file_path has no extension, try to guess from project context?
    // For now we keep the path as-is; the caller resolves it against project_root.

    Ok(ParsedSelector {
        file_path,
        kind,
        name,
    })
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
            if matches!(kind, SymbolKind::Impl) {
                if let Some(trait_name) = remainder.split_whitespace().next() {
                    return (kind, trait_name.to_string());
                }
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
pub fn resolve_file(selector: &ParsedSelector, project_root: &Path) -> Result<(PathBuf, String), String> {
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
        return Err(format!("cannot determine language from file: {}", full_path.display()));
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
        assert_eq!(sel.kind, SymbolKind::Function);
        assert_eq!(sel.name, "authenticate");
    }

    #[test]
    fn parse_fn_with_parens() {
        let sel = parse_selector("src/foo.rs::fn authenticate()").unwrap();
        assert_eq!(sel.name, "authenticate");
    }

    #[test]
    fn parse_struct_selector() {
        let sel = parse_selector("src/lib.rs::struct Config").unwrap();
        assert_eq!(sel.kind, SymbolKind::Struct);
        assert_eq!(sel.name, "Config");
    }

    #[test]
    fn parse_enum_selector() {
        let sel = parse_selector("src/types.rs::enum Color").unwrap();
        assert_eq!(sel.kind, SymbolKind::Enum);
        assert_eq!(sel.name, "Color");
    }

    #[test]
    fn parse_trait_selector() {
        let sel = parse_selector("src/lib.rs::trait Serialize").unwrap();
        assert_eq!(sel.kind, SymbolKind::Trait);
        assert_eq!(sel.name, "Serialize");
    }

    #[test]
    fn parse_impl_selector() {
        let sel = parse_selector("src/foo.rs::impl MyStruct").unwrap();
        assert_eq!(sel.kind, SymbolKind::Impl);
        assert_eq!(sel.name, "MyStruct");
    }

    #[test]
    fn parse_impl_for_selector() {
        let sel = parse_selector("src/foo.rs::impl Display for MyStruct").unwrap();
        assert_eq!(sel.kind, SymbolKind::Impl);
        assert_eq!(sel.name, "Display");
    }

    #[test]
    fn parse_bare_name() {
        let sel = parse_selector("src/foo.rs::authenticate").unwrap();
        assert_eq!(sel.kind, SymbolKind::Unknown);
        assert_eq!(sel.name, "authenticate");
    }

    #[test]
    fn parse_with_spaces_in_path() {
        // File paths with spaces are unusual but valid
        let sel = parse_selector("some dir/file.rs::fn hello").unwrap();
        assert_eq!(sel.file_path, PathBuf::from("some dir/file.rs"));
        assert_eq!(sel.name, "hello");
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
