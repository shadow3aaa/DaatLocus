use std::{collections::HashSet, sync::LazyLock};

use regex::Regex;
use serde_json::{Value, json};
use tracing::warn;

use crate::reasoning::runtime::AgentToolCall;

static DSML_FUNCTION_CALLS_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"<[｜|]DSML[｜|]function_calls>[\s\S]*?</?[｜|]DSML[｜|]function_calls>"#)
        .expect("DSML function_calls regex must compile")
});

static DSML_INVOKE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"<[｜|]DSML[｜|]invoke\s+name="([^"]+)">([\s\S]*?)</[｜|]DSML[｜|]invoke>"#)
        .expect("DSML invoke regex must compile")
});

static DSML_INVOKE_STRIP_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"<[｜|]DSML[｜|]invoke\s+[^>]*>[\s\S]*?</[｜|]DSML[｜|]invoke>"#)
        .expect("DSML invoke strip regex must compile")
});

static DSML_PARAMETER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"<[｜|]DSML[｜|]parameter\s+name="([^"]+)"(?:\s+string="(true|false)")?\s*>([\s\S]*?)</[｜|]DSML[｜|]parameter>"#,
    )
    .expect("DSML parameter regex must compile")
});

pub(crate) fn strip_dsml_from_thinking(thinking: &str) -> String {
    let mut out = thinking.to_string();

    out = DSML_FUNCTION_CALLS_RE.replace_all(&out, "").to_string();
    out = DSML_INVOKE_STRIP_RE.replace_all(&out, "").to_string();

    // Lone unpaired DSML opener left over after truncation mid-call.
    if let Some(i) = out.find("<｜DSML｜") {
        out.truncate(i);
    } else if let Some(i) = out.find("<|DSML|") {
        out.truncate(i);
    }

    out.trim().to_string()
}

pub(crate) fn scavenge_dsml_tool_calls(
    text: &str,
    allowed_tool_names: &HashSet<String>,
    max_calls: usize,
) -> Vec<AgentToolCall> {
    if text.is_empty() || allowed_tool_names.is_empty() || max_calls == 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut call_index = 0usize;

    for caps in DSML_INVOKE_RE.captures_iter(text) {
        if out.len() >= max_calls {
            break;
        }
        let name = caps.get(1).expect("capture group 1").as_str().to_string();
        if !allowed_tool_names.contains(&name) {
            continue;
        }
        let body = caps.get(2).expect("capture group 2").as_str();
        let id = format!("scavenged_{call_index}");
        call_index += 1;
        out.push(AgentToolCall {
            id,
            name,
            arguments: parse_dsml_parameters(body),
        });
    }

    let non_dsml = strip_dsml_blocks(text);
    for candidate_json in iterate_json_objects(&non_dsml) {
        if out.len() >= max_calls {
            break;
        }
        if let Some(call) =
            coerce_to_tool_call(&candidate_json, allowed_tool_names, &mut call_index)
        {
            out.push(call);
        }
    }

    if !out.is_empty() {
        warn!(
            "dsml repair: scavenged {} tool call(s) from thinking/content",
            out.len()
        );
    }
    out
}

fn parse_dsml_parameters(body: &str) -> Value {
    let mut args = serde_json::Map::new();
    for caps in DSML_PARAMETER_RE.captures_iter(body) {
        let key = caps.get(1).expect("parameter name").as_str();
        let string_flag = caps.get(2).map(|m| m.as_str());
        let raw = caps.get(3).map(|m| m.as_str().trim()).unwrap_or("");

        if string_flag == Some("false")
            && let Ok(parsed) = serde_json::from_str::<Value>(raw)
        {
            args.insert(key.to_string(), parsed);
            continue;
        }
        args.insert(key.to_string(), json!(raw));
    }
    Value::Object(args)
}

fn strip_dsml_blocks(text: &str) -> String {
    let out = DSML_FUNCTION_CALLS_RE.replace_all(text, "");
    DSML_INVOKE_STRIP_RE.replace_all(&out, "").to_string()
}

fn coerce_to_tool_call(
    candidate_json: &str,
    allowed_names: &HashSet<String>,
    call_index: &mut usize,
) -> Option<AgentToolCall> {
    let parsed: Value = serde_json::from_str(candidate_json).ok()?;
    if !parsed.is_object() {
        return None;
    }

    if let Some(name) = parsed.get("name").and_then(Value::as_str)
        && allowed_names.contains(name)
    {
        let arguments = match parsed.get("arguments") {
            Some(Value::String(s)) => serde_json::from_str(s).unwrap_or_else(|_| json!({})),
            Some(v) => v.clone(),
            None => json!({}),
        };
        let id = format!("scavenged_{call_index}");
        *call_index += 1;
        return Some(AgentToolCall {
            id,
            name: name.to_string(),
            arguments,
        });
    }

    if parsed.get("type").and_then(Value::as_str) == Some("function")
        && let Some(func) = parsed.get("function")
        && let Some(name) = func.get("name").and_then(Value::as_str)
        && allowed_names.contains(name)
    {
        let arguments = match func.get("arguments") {
            Some(Value::String(s)) => serde_json::from_str(s).unwrap_or_else(|_| json!({})),
            Some(v) => v.clone(),
            None => json!({}),
        };
        let id = format!("scavenged_{call_index}");
        *call_index += 1;
        return Some(AgentToolCall {
            id,
            name: name.to_string(),
            arguments,
        });
    }

    None
}

fn iterate_json_objects(text: &str) -> impl Iterator<Item = String> + '_ {
    JsonObjectIter { text, pos: 0 }
}

struct JsonObjectIter<'a> {
    text: &'a str,
    pos: usize,
}

impl<'a> Iterator for JsonObjectIter<'a> {
    type Item = String;

    fn next(&mut self) -> Option<Self::Item> {
        let bytes = self.text.as_bytes();
        while self.pos < self.text.len() {
            if bytes[self.pos] != b'{' {
                self.pos += 1;
                continue;
            }
            let start = self.pos;
            let mut depth = 0i32;
            let mut in_string = false;
            let mut escaped = false;
            for (j, &c) in bytes.iter().enumerate().skip(start) {
                if escaped {
                    escaped = false;
                    continue;
                }
                if in_string {
                    if c == b'\\' {
                        escaped = true;
                        continue;
                    }
                    if c == b'"' {
                        in_string = false;
                    }
                    continue;
                }
                if c == b'"' {
                    in_string = true;
                } else if c == b'{' {
                    depth += 1;
                } else if c == b'}' {
                    depth -= 1;
                    if depth == 0 {
                        let result = self.text[start..=j].to_string();
                        self.pos = j + 1;
                        return Some(result);
                    }
                }
            }
            self.pos = self.text.len();
            return None;
        }
        None
    }
}

pub(crate) fn repair_truncated_json(input: &str) -> TruncationRepairResult {
    if input.trim().is_empty() {
        return TruncationRepairResult {
            repaired: "{}".to_string(),
            changed: input != "{}",
            repaired_value: json!({}),
        };
    }
    if let Ok(value) = serde_json::from_str::<Value>(input) {
        return TruncationRepairResult {
            repaired: input.to_string(),
            changed: false,
            repaired_value: value,
        };
    }

    let bytes = input.as_bytes();
    let mut stack: Vec<BracketKind> = Vec::new();
    let mut in_string = false;
    let mut escaped = false;
    let mut last_significant = 0usize;

    for (i, &c) in bytes.iter().enumerate() {
        if !c.is_ascii_whitespace() {
            last_significant = i;
        }
        if escaped {
            escaped = false;
            continue;
        }
        if in_string {
            if c == b'\\' {
                escaped = true;
                continue;
            }
            if c == b'"' {
                in_string = false;
                stack.pop();
            }
            continue;
        }
        if c == b'"' {
            in_string = true;
            stack.push(BracketKind::String);
        } else if c == b'{' {
            stack.push(BracketKind::Object);
        } else if c == b'[' {
            stack.push(BracketKind::Array);
        } else if c == b'}' {
            if stack.last() == Some(&BracketKind::Object) {
                stack.pop();
            }
        } else if c == b']' && stack.last() == Some(&BracketKind::Array) {
            stack.pop();
        }
    }

    let mut s = input[..=last_significant].to_string();

    if s.ends_with(',') {
        s.pop();
    }

    if s.ends_with(':') || s.ends_with(": ") {
        s.push_str("null");
    }

    if in_string {
        s.push('"');
        if stack.last() == Some(&BracketKind::String) {
            stack.pop();
        }
    }

    while let Some(top) = stack.pop() {
        match top {
            BracketKind::Object => s.push('}'),
            BracketKind::Array => s.push(']'),
            BracketKind::String => s.push('"'),
        }
    }

    match serde_json::from_str::<Value>(&s) {
        Ok(value) => TruncationRepairResult {
            repaired: s,
            changed: true,
            repaired_value: value,
        },
        Err(_) => {
            warn!("dsml repair: truncated JSON repair failed, falling back to {{}}");
            TruncationRepairResult {
                repaired: "{}".to_string(),
                changed: true,
                repaired_value: json!({}),
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BracketKind {
    Object,
    Array,
    String,
}

pub(crate) struct TruncationRepairResult {
    pub repaired: String,
    pub changed: bool,
    pub repaired_value: Value,
}

pub(crate) fn repair_tool_call_arguments(call: &mut AgentToolCall) -> bool {
    let args_str = match call.arguments {
        Value::String(ref s) => s.clone(),
        _ => {
            let s = call.arguments.to_string();
            if s == "null" || s.is_empty() {
                call.arguments = json!({});
                return true;
            }
            s
        }
    };
    if serde_json::from_str::<Value>(&args_str).is_ok() {
        return false;
    }
    let result = repair_truncated_json(&args_str);
    if result.changed {
        warn!(
            "dsml repair: repaired truncated JSON for tool call {} ({} chars → {} chars)",
            call.name,
            args_str.len(),
            result.repaired.len()
        );
        call.arguments = result.repaired_value;
    }
    result.changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_dsml_removes_full_width_envelope() {
        let input = "I need to call a tool\n<｜DSML｜function_calls>\nstuff\n</｜DSML｜function_calls>\nDone thinking.";
        let result = strip_dsml_from_thinking(input);
        assert!(!result.contains("DSML"));
        assert!(result.contains("I need to call a tool"));
        assert!(result.contains("Done thinking."));
    }

    #[test]
    fn strip_dsml_removes_pipe_envelope() {
        let input = "Think\n<|DSML|function_calls>data</|DSML|function_calls>\nEnd";
        let result = strip_dsml_from_thinking(input);
        assert!(!result.contains("DSML"));
    }

    #[test]
    fn strip_dsml_truncates_lone_opener() {
        let input = "Reasoning<｜DSML｜more stuff here";
        let result = strip_dsml_from_thinking(input);
        assert_eq!(result, "Reasoning");
    }

    #[test]
    fn strip_dsml_removes_lone_invoke() {
        let input = "Before\n<｜DSML｜invoke name=\"x\">\n<｜DSML｜parameter name=\"p\">v</｜DSML｜parameter>\n</｜DSML｜invoke>\nAfter";
        let result = strip_dsml_from_thinking(input);
        assert!(!result.contains("DSML"));
        assert!(result.contains("Before"));
        assert!(result.contains("After"));
    }

    #[test]
    fn scavenge_dsml_invoke_block() {
        let text = r#"<｜DSML｜invoke name="read_file">
<｜DSML｜parameter name="path">/tmp/test.txt</｜DSML｜parameter>
</｜DSML｜invoke>"#;
        let mut allowed = HashSet::new();
        allowed.insert("read_file".to_string());
        let calls = scavenge_dsml_tool_calls(text, &allowed, 4);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "read_file");
        assert_eq!(calls[0].arguments["path"], "/tmp/test.txt");
        assert!(calls[0].id.starts_with("scavenged_"));
    }

    #[test]
    fn scavenge_skips_unknown_tools() {
        let text = r#"<｜DSML｜invoke name="unknown_tool">
<｜DSML｜parameter name="x">1</｜DSML｜parameter>
</｜DSML｜invoke>"#;
        let mut allowed = HashSet::new();
        allowed.insert("read_file".to_string());
        let calls = scavenge_dsml_tool_calls(text, &allowed, 4);
        assert!(calls.is_empty());
    }

    #[test]
    fn scavenge_raw_json_name_arguments() {
        let text = r#"{"name": "read_file", "arguments": {"path": "/tmp/x"}}"#;
        let mut allowed = HashSet::new();
        allowed.insert("read_file".to_string());
        let calls = scavenge_dsml_tool_calls(text, &allowed, 4);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "read_file");
        assert_eq!(calls[0].arguments["path"], "/tmp/x");
    }

    #[test]
    fn scavenge_raw_json_openai_style() {
        let text = r#"{"type": "function", "function": {"name": "write_file", "arguments": {"path": "/a", "content": "hi"}}}"#;
        let mut allowed = HashSet::new();
        allowed.insert("write_file".to_string());
        let calls = scavenge_dsml_tool_calls(text, &allowed, 4);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "write_file");
    }

    #[test]
    fn scavenge_respects_max_calls() {
        let text = r#"<｜DSML｜invoke name="a">
</｜DSML｜invoke>
<｜DSML｜invoke name="b">
</｜DSML｜invoke>
<｜DSML｜invoke name="c">
</｜DSML｜invoke>"#;
        let mut allowed = HashSet::new();
        allowed.insert("a".to_string());
        allowed.insert("b".to_string());
        allowed.insert("c".to_string());
        let calls = scavenge_dsml_tool_calls(text, &allowed, 2);
        assert_eq!(calls.len(), 2);
    }

    #[test]
    fn repair_truncated_balances_braces() {
        let input = r#"{"path": "/tmp/test"#;
        let result = repair_truncated_json(input);
        assert!(result.changed);
        assert!(serde_json::from_str::<Value>(&result.repaired).is_ok());
    }

    #[test]
    fn repair_truncated_trailing_comma() {
        let input = r#"{"path": "/tmp/test",}"#;
        let result = repair_truncated_json(input);
        assert!(result.changed);
        assert!(serde_json::from_str::<Value>(&result.repaired).is_ok());
    }

    #[test]
    fn repair_truncated_dangling_key() {
        let input = r#"{"path":"#;
        let result = repair_truncated_json(input);
        assert!(result.changed);
        let parsed = serde_json::from_str::<Value>(&result.repaired).unwrap();
        assert_eq!(parsed["path"], Value::Null);
    }

    #[test]
    fn repair_truncated_empty_input() {
        let result = repair_truncated_json("");
        assert!(result.changed);
        assert_eq!(result.repaired_value, json!({}));
    }

    #[test]
    fn repair_valid_json_is_noop() {
        let input = r#"{"path": "/tmp/test"}"#;
        let result = repair_truncated_json(input);
        assert!(!result.changed);
        assert_eq!(result.repaired, input);
    }

    #[test]
    fn repair_tool_call_arguments_valid_is_noop() {
        let mut call = AgentToolCall {
            id: "call_0".to_string(),
            name: "test".to_string(),
            arguments: json!({"key": "value"}),
        };
        assert!(!repair_tool_call_arguments(&mut call));
    }

    #[test]
    fn repair_tool_call_arguments_truncated() {
        let mut call = AgentToolCall {
            id: "call_0".to_string(),
            name: "test".to_string(),
            arguments: Value::String(r#"{"path": "/tmp/test"#.to_string()),
        };
        assert!(repair_tool_call_arguments(&mut call));
        assert!(call.arguments.is_object());
    }

    #[test]
    fn strip_and_scavenge_combined() {
        let thinking = r#"I should read the file.
<｜DSML｜function_calls>
<｜DSML｜invoke name="read_file">
<｜DSML｜parameter name="path">/tmp/x</｜DSML｜parameter>
</｜DSML｜invoke>
</｜DSML｜function_calls>
Now I will proceed."#;
        let stripped = strip_dsml_from_thinking(thinking);
        assert!(!stripped.contains("DSML"));

        let mut allowed = HashSet::new();
        allowed.insert("read_file".to_string());
        let calls = scavenge_dsml_tool_calls(thinking, &allowed, 4);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "read_file");
    }

    #[test]
    fn strip_dsml_with_ascii_pipe() {
        let input = "Think\n<|DSML|function_calls>data</|DSML|function_calls>\nEnd";
        let result = strip_dsml_from_thinking(input);
        assert!(!result.contains("DSML"));
        assert!(result.contains("Think"));
        assert!(result.contains("End"));
    }

    #[test]
    fn scavenge_dsml_invoke_with_ascii_pipe() {
        let text = r#"<|DSML|invoke name="read_file">
<|DSML|parameter name="path">/tmp/test.txt</|DSML|parameter>
</|DSML|invoke>"#;
        let mut allowed = HashSet::new();
        allowed.insert("read_file".to_string());
        let calls = scavenge_dsml_tool_calls(text, &allowed, 4);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "read_file");
    }
}
