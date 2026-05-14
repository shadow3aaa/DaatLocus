use crate::api::{ScopeSelectorKindSchema, ScopeUsageResponse};

const SCOPE_USAGE_MD: &str = include_str!("../USAGE.md");

/// Return the current SCOPE usage documentation compiled with the crate.
pub fn usage_markdown() -> &'static str {
    SCOPE_USAGE_MD
}

/// Return SCOPE selector kinds and operation support as machine-readable data.
pub fn selector_schema() -> Vec<ScopeSelectorKindSchema> {
    vec![
        ScopeSelectorKindSchema {
            kind: "symbol".to_string(),
            syntax: "src/foo.rs::Bar::new".to_string(),
            read: true,
            edit: true,
            delete: true,
            notes: "Symbol selectors resolve one AST definition and support semantic edit/delete with normal propagation analysis.".to_string(),
        },
        ScopeSelectorKindSchema {
            kind: "line_range".to_string(),
            syntax: "src/foo.rs#L120-L180".to_string(),
            read: true,
            edit: true,
            delete: false,
            notes: "File ranges locate explicit line spans. Edits are patch-only and must be followed by affected-symbol analysis.".to_string(),
        },
        ScopeSelectorKindSchema {
            kind: "around_line".to_string(),
            syntax: "src/foo.rs#around:L150±40".to_string(),
            read: true,
            edit: false,
            delete: false,
            notes: "Around selectors are context windows for reading; resolve to a bounded file range.".to_string(),
        },
        ScopeSelectorKindSchema {
            kind: "match".to_string(),
            syntax: "src/foo.rs#match:/ProjectInstructions/".to_string(),
            read: true,
            edit: true,
            delete: false,
            notes: "Match selectors locate regex hits. Edits require exactly one match; multiple matches must return candidates.".to_string(),
        },
        ScopeSelectorKindSchema {
            kind: "match_around".to_string(),
            syntax: "src/foo.rs#match:/ProjectInstructions/#around:40".to_string(),
            read: true,
            edit: false,
            delete: false,
            notes: "Match-around selectors read context around a unique regex match.".to_string(),
        },
        ScopeSelectorKindSchema {
            kind: "enclosing".to_string(),
            syntax: "src/foo.rs#enclosing:L150".to_string(),
            read: true,
            edit: true,
            delete: false,
            notes: "Enclosing selectors resolve the innermost symbol containing a line, then use symbol semantics.".to_string(),
        },
        ScopeSelectorKindSchema {
            kind: "outline".to_string(),
            syntax: "src/foo.rs#outline".to_string(),
            read: true,
            edit: false,
            delete: false,
            notes: "Outline selectors return file structure only and are read-only.".to_string(),
        },
    ]
}

/// Return both human-readable and machine-readable SCOPE usage data.
pub fn usage_response() -> ScopeUsageResponse {
    ScopeUsageResponse {
        usage_markdown: usage_markdown().to_string(),
        selector_kinds: selector_schema(),
    }
}
