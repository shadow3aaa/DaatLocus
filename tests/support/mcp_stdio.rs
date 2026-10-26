//! Minimal MCP server for host access, reviewed elevation and descendant cleanup.
use std::io::{self, BufRead, Write};
fn main() {
    let paths: Vec<_> = std::env::args_os().skip(1).collect();
    if paths.first().is_some_and(|arg| arg == "--descendant") {
        std::thread::sleep(std::time::Duration::from_secs(120));
        return;
    }
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .arg("--descendant")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let descendant = command.spawn().unwrap();
    std::fs::write(
        std::path::Path::new(&paths[2]).with_extension("child.pid"),
        descendant.id().to_string(),
    )
    .unwrap();
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
            let mut text =
                std::fs::read_to_string(&paths[index]).unwrap_or_else(|_| "denied".into());
            if index == 1 {
                text.push_str(if std::fs::write(&paths[index], "bad").is_err() {
                    "|write-denied"
                } else {
                    "|write-allowed"
                });
            }
            let value = std::env::var("DL_MCP_FIXTURE_TOKEN").unwrap();
            let ambient = std::env::var("DL_MCP_AMBIENT_SECRET").unwrap();
            format!(
                r#"{{"content":[{{"type":"text","text":"{value}|{ambient}|{text}"}}],"isError":false}}"#
            )
        } else {
            panic!("unexpected request");
        };
        println!(r#"{{"jsonrpc":"2.0","id":{id},"result":{result}}}"#);
        io::stdout().flush().unwrap();
    }
}
