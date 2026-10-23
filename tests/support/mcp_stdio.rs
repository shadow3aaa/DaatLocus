//! Minimal native MCP server for the Windows sandbox/pipe integration test.
use std::io::{self, BufRead, Write};
fn main() {
    let paths: Vec<_> = std::env::args_os().skip(1).collect();
    std::fs::write(&paths[2], std::process::id().to_string()).unwrap();
    for line in io::stdin().lock().lines() {
        let line = line.unwrap();
        let Some(after) = line.split("\"id\":").nth(1) else {
            continue;
        };
        let id: String = after
            .trim_start()
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        let result = if line.contains("\"method\":\"initialize\"") {
            let version = line
                .split("\"protocolVersion\":\"")
                .nth(1)
                .unwrap()
                .split('"')
                .next()
                .unwrap();
            format!(
                r#"{{"protocolVersion":"{version}","capabilities":{{"tools":{{}}}},"serverInfo":{{"name":"fixture","version":"1"}}}}"#
            )
        } else if line.contains("\"method\":\"tools/list\"") {
            r#"{"tools":[{"name":"read","inputSchema":{"type":"object","properties":{"value":{"type":"boolean"}},"required":["value"],"additionalProperties":false}}]}"#.to_owned()
        } else if line.contains("\"method\":\"tools/call\"") {
            let index = if line.contains("\"value\":true") {
                0
            } else {
                1
            };
            let mut text = std::fs::read_to_string(&paths[index]).unwrap_or_else(|_| "denied".into());
            if index == 1 {
                text.push_str(if std::fs::write(&paths[index], "bad").is_err() {
                    "|write-denied"
                } else {
                    "|write-allowed"
                });
            }
            let value = std::env::var("DL_MCP_FIXTURE_TOKEN").unwrap();
            let ambient = std::env::var("DL_MCP_AMBIENT_SECRET").unwrap();
            format!(r#"{{"content":[{{"type":"text","text":"{value}|{ambient}|{text}"}}],"isError":false}}"#)
        } else {
            panic!("unexpected request");
        };
        println!(r#"{{"jsonrpc":"2.0","id":{id},"result":{result}}}"#);
        io::stdout().flush().unwrap();
    }
}
