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
            syntax: r#"{"query":"content","path":"src","include":"*.rs","limit":20}"#
                .to_string(),
            notes: "Searches source content and returns stable read handles plus canonical target labels."
                .to_string(),
        },
        ScopeProtocolItemSchema {
            item: "read_code_ref".to_string(),
            syntax: r##"{"ref":"1268#k7Qp"}"##.to_string(),
            notes: "Reads a target located by a stable search handle and returns only hash-anchored source lines."
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
