use crate::api::{ScopeProtocolItemSchema, ScopeUsageResponse};

const SCOPE_USAGE_MD: &str = include_str!("../USAGE.md");

/// Return the current SCOPE usage documentation compiled with the crate.
pub fn usage_markdown() -> &'static str {
    SCOPE_USAGE_MD
}

/// Return SCOPE's model-facing protocol surface as machine-readable data.
pub fn protocol_schema() -> Vec<ScopeProtocolItemSchema> {
    vec![
        ScopeProtocolItemSchema {
            item: "search_code".to_string(),
            syntax: r#"{"query":"matching_commands(","mode":"literal","path":"src/dashboard","include":["*.rs"],"exclude":["target/**"],"types":["rust"],"case":"smart","word":false,"line":false,"hidden":false,"respect_ignore":true,"follow":false,"limit":20}"#
                .to_string(),
            notes: "Searches source content and returns matched-line hits with path plus line-hash anchors. Query defaults to literal matching with smart case; use mode:\"regex\" only for regular expressions."
                .to_string(),
        },
        ScopeProtocolItemSchema {
            item: "read_code".to_string(),
            syntax: r##"{"path":"src/foo.rs","anchor":"42#ab","mode":"full"}"##.to_string(),
            notes: "Reads a path plus line-hash anchor in around or full mode and returns only hash-anchored source lines."
                .to_string(),
        },
        ScopeProtocolItemSchema {
            item: "edit_code".to_string(),
            syntax: r#"{"edits":[{"path":"src/foo.rs","op":"replace","start":"10#7a","end":"20#d4","content":"..."}]}"#
                .to_string(),
            notes: "Applies path plus line-hash anchored Replace/Append/Prepend edits and returns propagation results."
                .to_string(),
        },
    ]
}

/// Return both human-readable and machine-readable SCOPE usage data.
pub fn usage_response() -> ScopeUsageResponse {
    ScopeUsageResponse {
        usage_markdown: usage_markdown().to_string(),
        protocol_items: protocol_schema(),
    }
}
