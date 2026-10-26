//! Process-only Rubash host. This entry never initializes an Agent, account,
//! daemon, Tokio runtime or profile. Internal respawns use this same executable.
use rubash::{Executor, invocation::ShellInvocation, script_driver};
use std::io::{self, BufRead, Read, Write};

/// Called before the embedding service parses CLI arguments or opens storage.
/// The environment marker routes only Rubash's own shell-shaped respawns; it
/// conveys no authority and never bypasses the process sandbox.
pub fn try_run() {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let explicit = args.first().is_some_and(|arg| arg == "--internal-bash");
    // These options do not belong to the maintenance CLI. Recognize them
    // without an environment marker so `env -i "$BASH" -c ...` also works.
    let shell_flag = args.first().is_some_and(|arg| {
        arg.to_str().is_some_and(|arg| {
            (arg.starts_with('-') && !arg.starts_with("--") && !matches!(arg, "-h" | "-V"))
                || matches!(
                    arg,
                    "--posix" | "--noprofile" | "--norc" | "--login" | "--rcfile" | "--init-file"
                )
                || arg.starts_with("--internal-")
        })
    });
    let inherited_shell = std::env::var_os("DAAT_LOCUS_BASH_HOST").as_deref()
        == Some(std::ffi::OsStr::new("1"))
        && args
            .first()
            .is_none_or(|arg| std::path::Path::new(arg).is_file());
    if explicit || shell_flag || inherited_shell {
        let args = args
            .into_iter()
            .skip(usize::from(explicit))
            .map(|arg| {
                arg.into_string().unwrap_or_else(|_| {
                    eprintln!("Bash arguments must be Unicode");
                    std::process::exit(2)
                })
            })
            .collect();
        run_process(args);
    }
}

fn run_process(arguments: Vec<String>) -> ! {
    // This entry runs in the isolated shell process before starting any threads.
    unsafe {
        std::env::set_var("RUBASH_IDENTITY", "native");
        std::env::remove_var("CARGO_BIN_EXE_rubash");
        std::env::remove_var("NIU_INVOKED_AS");
        std::env::set_var("DAAT_LOCUS_BASH_HOST", "1");
    }
    // Rust's stdout buffers partial lines when connected to a pipe. Shell
    // builtins must publish prompts/markers even while `read` blocks on stdin.
    std::thread::spawn(|| {
        loop {
            let _ = io::stdout().flush();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    });
    let result = std::thread::Builder::new()
        .name("terminal-shell".into())
        .stack_size(512 * 1024 * 1024)
        .spawn(move || run(arguments))
        .and_then(|thread| {
            thread
                .join()
                .map_err(|_| io::Error::other("Shell thread panicked"))
        });
    let code = match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!("daat-locus-bash: {error}");
            1
        }
    };
    std::process::exit(code)
}

fn run(arguments: Vec<String>) -> i32 {
    rubash::locale::init_locale();
    let mut args: Vec<String> = vec![std::env::args().next().unwrap_or_default()];
    args.extend(arguments);
    rubash::invocation::materialize_command_file(&mut args);
    rubash::builtins::kill::consume_background_wrapper_handshake(&mut args);
    if let Some(name) = args.get(1).and_then(|arg| arg.strip_prefix("--internal-")) {
        return internal_pipeline(name, &args[2..]);
    }
    let invocation = match ShellInvocation::parse(&args[1..]) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("daat-locus-bash: {error}");
            return 2;
        }
    };
    let mut executor = Executor::new_process_exit();
    if let Ok(path) = std::env::current_exe() {
        let display = rubash::executor::shell_pwd_display_path(&path.to_string_lossy());
        executor.export_env("BASH", &display);
        executor.export_env("SHELL", &display);
        executor.set_compatible_shell_path(path);
    }
    if let Err(error) = invocation.apply_to_executor(&mut executor) {
        eprintln!("daat-locus-bash: {error}");
        return 2;
    }
    executor.inherit_process_stdin();
    let status = if let Some(command) = &invocation.command {
        executor.set_command_string_mode(true);
        let command =
            if rubash::executor::RESPAWNED_CHILD.load(std::sync::atomic::Ordering::Relaxed) {
                std::borrow::Cow::Borrowed(command.as_str())
            } else {
                std::borrow::Cow::Owned(script_driver::command_string_entry_text(command))
            };
        let offset = executor
            .get_env("__RUBASH_LINE_OFFSET")
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        if script_driver::script_uses_history(&command)
            || script_driver::script_uses_aliases(&command, executor.alias_expansion_enabled())
        {
            script_driver::run_script_with_history_at_line_offset(
                &mut executor,
                &command,
                offset,
                None,
            )
        } else {
            script_driver::run_source_with_line_offset(
                &mut executor,
                &command,
                false,
                offset,
                None,
                None,
            )
        }
    } else if let Some(script) = &invocation.script {
        let path = executor.resolve_shell_path(script);
        let source = match script_driver::read_script_bytes(&path) {
            Ok(source) => source,
            Err(error) => {
                eprintln!("daat-locus-bash: {script}: {error}");
                return if error.kind() == io::ErrorKind::NotFound {
                    127
                } else {
                    126
                };
            }
        };
        executor.set_env("__RUBASH_SCRIPT_NAME", script);
        executor.set_env("BASH_ARGV0", script);
        script_driver::run_script_with_history(&mut executor, &source, None)
    } else {
        script_driver::run_stdin_script(&mut executor)
    };
    script_driver::finish_shell(&mut executor, status, false)
}

// Rubash's streaming pipeline optimization reexecutes its host with these
// internal operations. Keep them streaming so `yes | head` remains bounded.
fn internal_pipeline(name: &str, args: &[String]) -> i32 {
    let mut output = io::stdout().lock();
    match name {
        "yes" => {
            let line = if args.is_empty() {
                "y".into()
            } else {
                args.join(" ")
            };
            let chunk = format!("{line}\n").repeat(256);
            while output
                .write_all(chunk.as_bytes())
                .and_then(|()| output.flush())
                .is_ok()
            {}
        }
        "head" => {
            let count = args
                .iter()
                .enumerate()
                .find_map(|(index, arg)| {
                    if arg == "-n" {
                        args.get(index + 1)?.parse().ok()
                    } else {
                        arg.strip_prefix("--lines=")
                            .or_else(|| arg.strip_prefix("-n"))
                            .or_else(|| arg.strip_prefix('-'))?
                            .parse()
                            .ok()
                    }
                })
                .unwrap_or(10usize);
            let mut input = io::stdin().lock();
            let mut line = Vec::new();
            for _ in 0..count {
                line.clear();
                match input.read_until(b'\n', &mut line) {
                    Ok(0) => break,
                    Ok(_) => {
                        if output.write_all(&line).is_err() {
                            break;
                        }
                    }
                    Err(_) => return 1,
                }
            }
        }
        "wc" => {
            let mut input = io::stdin().lock();
            let mut buffer = [0; 8192];
            let mut lines = 0usize;
            loop {
                match input.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(count) => {
                        lines += buffer[..count]
                            .iter()
                            .filter(|byte| **byte == b'\n')
                            .count()
                    }
                    Err(_) => return 1,
                }
            }
            if writeln!(output, "{lines}").is_err() {
                return 1;
            }
        }
        _ => return 127,
    }
    let _ = output.flush();
    0
}
