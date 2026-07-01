use std::collections::HashSet;

use crate::reasoning::runtime::AgentToolCall;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct CodingSourceLineKey {
    pub(crate) path: String,
    pub(crate) anchor: String,
}

#[derive(Debug, Clone)]
struct CodingSourceRecord<'a> {
    path: Option<&'a str>,
    anchor: &'a str,
}

#[derive(Debug, Clone)]
struct CodingOmittedSpan {
    path: Option<String>,
    start_anchor: String,
    end_anchor: String,
    end_line_number: usize,
    len: usize,
}

#[derive(Debug)]
struct CodingSourceElider<'a> {
    visible_full_lines: &'a mut HashSet<CodingSourceLineKey>,
}

pub(super) fn elide_tool_model_content(
    visible_lines: &mut HashSet<CodingSourceLineKey>,
    call: &AgentToolCall,
    content: &str,
) -> String {
    let mut elider = CodingSourceElider {
        visible_full_lines: visible_lines,
    };
    elider.elide_tool_model_content(call, content)
}

impl CodingSourceElider<'_> {
    fn elide_tool_model_content(&mut self, call: &AgentToolCall, content: &str) -> String {
        if is_line_hash_read_tool(&call.name)
            && let Some(path) = line_hash_call_path(call)
        {
            return self.elide_read_content(&path, content);
        }
        if is_coding_search_code_tool(&call.name) {
            return self.elide_search_content(content);
        }
        content.to_string()
    }

    fn elide_read_content(&mut self, path: &str, content: &str) -> String {
        let mut lines = Vec::new();
        let mut span = OmittedSpanBuilder::default();
        for line in content.lines() {
            let Some(record) = parse_line_hash_read_full_record(line) else {
                span.flush_read(&mut lines);
                lines.push(line.to_string());
                continue;
            };
            let line_number = anchor_line_number(record.anchor);
            if self.is_visible(path, record.anchor) {
                span.push(None, record.anchor, line_number, &mut lines);
            } else {
                span.flush_read(&mut lines);
                lines.push(line.to_string());
                self.mark_visible(path, record.anchor);
            }
        }
        span.flush_read(&mut lines);
        preserve_trailing_newline(lines.join("\n"), content)
    }

    fn elide_search_content(&mut self, content: &str) -> String {
        let mut lines = Vec::new();
        let mut span = OmittedSpanBuilder::default();
        for line in content.lines() {
            let Some(record) = parse_coding_search_full_record(line) else {
                span.flush_search(&mut lines);
                lines.push(line.to_string());
                continue;
            };
            let path = record.path.expect("search record path");
            let line_number = anchor_line_number(record.anchor);
            if self.is_visible(path, record.anchor) {
                span.push(Some(path), record.anchor, line_number, &mut lines);
            } else {
                span.flush_search(&mut lines);
                lines.push(line.to_string());
                self.mark_visible(path, record.anchor);
            }
        }
        span.flush_search(&mut lines);
        preserve_trailing_newline(lines.join("\n"), content)
    }

    fn is_visible(&self, path: &str, anchor: &str) -> bool {
        self.visible_full_lines.contains(&CodingSourceLineKey {
            path: path.to_string(),
            anchor: anchor.to_string(),
        })
    }

    fn mark_visible(&mut self, path: &str, anchor: &str) {
        self.visible_full_lines.insert(CodingSourceLineKey {
            path: path.to_string(),
            anchor: anchor.to_string(),
        });
    }
}

#[derive(Debug, Default)]
struct OmittedSpanBuilder {
    span: Option<CodingOmittedSpan>,
}

impl OmittedSpanBuilder {
    fn push(
        &mut self,
        path: Option<&str>,
        anchor: &str,
        line_number: Option<usize>,
        output: &mut Vec<String>,
    ) {
        let path_string = path.map(ToString::to_string);
        let can_extend = self.span.as_ref().is_some_and(|span| {
            span.path == path_string
                && line_number
                    .zip(Some(span.end_line_number))
                    .is_some_and(|(current, previous)| current == previous + 1)
        });
        if can_extend {
            if let Some(span) = &mut self.span {
                span.end_anchor = anchor.to_string();
                if let Some(line_number) = line_number {
                    span.end_line_number = line_number;
                }
                span.len += 1;
            }
            return;
        }
        if path.is_some() {
            self.flush_search(output);
        } else {
            self.flush_read(output);
        }
        self.span = Some(CodingOmittedSpan {
            path: path_string,
            start_anchor: anchor.to_string(),
            end_anchor: anchor.to_string(),
            end_line_number: line_number.unwrap_or(usize::MAX),
            len: 1,
        });
    }

    fn flush_read(&mut self, output: &mut Vec<String>) {
        let Some(span) = self.span.take() else {
            return;
        };
        output.push(render_omitted_record(None, &span));
    }

    fn flush_search(&mut self, output: &mut Vec<String>) {
        let Some(span) = self.span.take() else {
            return;
        };
        output.push(render_omitted_record(span.path.as_deref(), &span));
    }
}

fn render_omitted_record(path: Option<&str>, span: &CodingOmittedSpan) -> String {
    let record = if span.len == 1 {
        format!("{}~", span.start_anchor)
    } else {
        format!("{}...{}~", span.start_anchor, span.end_anchor)
    };
    match path {
        Some(path) => format!("{path}|{record}"),
        None => record,
    }
}

fn preserve_trailing_newline(mut output: String, original: &str) -> String {
    if original.ends_with('\n') && !output.ends_with('\n') {
        output.push('\n');
    }
    output
}

fn is_line_hash_read_tool(name: &str) -> bool {
    is_coding_read_code_tool(name) || is_read_file_tool(name)
}

fn is_coding_read_code_tool(name: &str) -> bool {
    name == "coding__read_code" || name == "read_code"
}

fn is_read_file_tool(name: &str) -> bool {
    name == "read_file"
}

fn is_coding_search_code_tool(name: &str) -> bool {
    name == "coding__search_code" || name == "search_code"
}

fn line_hash_call_path(call: &AgentToolCall) -> Option<String> {
    call.arguments
        .get("path")
        .and_then(|value| value.as_str())
        .map(ToString::to_string)
}

fn parse_line_hash_read_full_record(line: &str) -> Option<CodingSourceRecord<'_>> {
    let (anchor, _source) = line.split_once('|')?;
    parse_coding_anchor(anchor)?;
    Some(CodingSourceRecord { path: None, anchor })
}

fn parse_coding_search_full_record(line: &str) -> Option<CodingSourceRecord<'_>> {
    let (path, rest) = line.split_once('|')?;
    let (anchor, _source) = rest.split_once('|')?;
    if path.is_empty() {
        return None;
    }
    parse_coding_anchor(anchor)?;
    Some(CodingSourceRecord {
        path: Some(path),
        anchor,
    })
}

fn parse_coding_anchor(anchor: &str) -> Option<(usize, &str)> {
    let (line, hash) = anchor.split_once('#')?;
    if line.is_empty()
        || hash.is_empty()
        || !line.bytes().all(|byte| byte.is_ascii_digit())
        || hash
            .bytes()
            .any(|byte| matches!(byte, b'|' | b'~' | b'.') || byte.is_ascii_whitespace())
    {
        return None;
    }
    let line = line.parse::<usize>().ok()?;
    Some((line, hash))
}

fn anchor_line_number(anchor: &str) -> Option<usize> {
    parse_coding_anchor(anchor).map(|(line, _)| line)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_visible(path: &str, anchors: &[&str]) -> HashSet<CodingSourceLineKey> {
        anchors
            .iter()
            .map(|anchor| CodingSourceLineKey {
                path: path.to_string(),
                anchor: anchor.to_string(),
            })
            .collect()
    }

    #[test]
    fn collapses_repeated_read_lines() {
        let mut visible = make_visible("src/foo.rs", &["10#aa", "11#bb", "12#cc"]);
        let call = AgentToolCall {
            id: "call_read_2".to_string(),
            name: "coding__read_code".to_string(),
            arguments: serde_json::json!({
                "path": "src/foo.rs",
                "anchor": "10#aa",
                "mode": "around",
            }),
        };

        let output = elide_tool_model_content(
            &mut visible,
            &call,
            "10#aa|let a = 1;\n11#bb|\n12#cc|let b = 2;\n13#dd|let c = 3;\n",
        );

        assert_eq!(output, "10#aa...12#cc~\n13#dd|let c = 3;\n");
        // Newly seen line is now visible
        assert!(visible.contains(&CodingSourceLineKey {
            path: "src/foo.rs".into(),
            anchor: "13#dd".into(),
        }));
    }

    #[test]
    fn collapses_repeated_read_file_lines() {
        let mut visible = make_visible("AGENTS.md", &["1#aa", "2#bb"]);
        let call = AgentToolCall {
            id: "call_file_2".to_string(),
            name: "read_file".to_string(),
            arguments: serde_json::json!({
                "path": "AGENTS.md",
                "start_line": 1,
                "line_count": 4,
            }),
        };

        let output =
            elide_tool_model_content(&mut visible, &call, "1#aa|# Title\n2#bb|\n3#cc|More text\n");

        assert_eq!(output, "1#aa...2#bb~\n3#cc|More text\n");
    }

    #[test]
    fn collapses_search_lines_per_path_and_adjacency() {
        let mut visible = make_visible("src/foo.rs", &["20#aa", "21#bb", "23#cc"]);
        visible.extend(make_visible("src/bar.rs", &["7#dd"]));
        let call = AgentToolCall {
            id: "call_search_2".to_string(),
            name: "coding__search_code".to_string(),
            arguments: serde_json::json!({
                "query": "unused",
                "mode": "literal",
                "path": null,
                "include": [],
                "exclude": [],
                "types": [],
                "type_not": [],
                "case": "smart",
                "word": false,
                "whole_line": false,
                "hidden": false,
                "respect_ignore": true,
                "follow": false,
                "limit": 20,
            }),
        };

        let output = elide_tool_model_content(
            &mut visible,
            &call,
            "src/foo.rs|20#aa|foo\nsrc/foo.rs|21#bb|bar\nsrc/foo.rs|23#cc|gap\nsrc/foo.rs|24#ee|fresh\nsrc/bar.rs|7#dd|baz\nsrc/bar.rs|8#ff|fresh\n",
        );

        assert_eq!(
            output,
            "src/foo.rs|20#aa...21#bb~\nsrc/foo.rs|23#cc~\nsrc/foo.rs|24#ee|fresh\nsrc/bar.rs|7#dd~\nsrc/bar.rs|8#ff|fresh\n"
        );
    }

    #[test]
    fn shares_visibility_between_search_and_read() {
        let mut visible = make_visible("src/foo.rs", &["42#ab"]);
        let call = AgentToolCall {
            id: "call_read_1".to_string(),
            name: "coding__read_code".to_string(),
            arguments: serde_json::json!({
                "path": "src/foo.rs",
                "anchor": "42#ab",
                "mode": "around",
            }),
        };

        let output = elide_tool_model_content(
            &mut visible,
            &call,
            "41#aa|fn wrapper() {\n42#ab|    call_target();\n43#ac|}\n",
        );

        assert_eq!(output, "41#aa|fn wrapper() {\n42#ab~\n43#ac|}\n");
    }

    #[test]
    fn does_not_treat_omitted_records_as_visible_source() {
        let mut visible = HashSet::new();
        // Only the ~ form was in history — should NOT be treated as visible
        let call = AgentToolCall {
            id: "call_search_2".to_string(),
            name: "coding__search_code".to_string(),
            arguments: serde_json::json!({
                "query": "unused",
                "mode": "literal",
                "path": null,
                "include": [],
                "exclude": [],
                "types": [],
                "type_not": [],
                "case": "smart",
                "word": false,
                "whole_line": false,
                "hidden": false,
                "respect_ignore": true,
                "follow": false,
                "limit": 20,
            }),
        };

        let output =
            elide_tool_model_content(&mut visible, &call, "src/foo.rs|42#ab|    call_target();");

        assert_eq!(output, "src/foo.rs|42#ab|    call_target();");
    }
}
