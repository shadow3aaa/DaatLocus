# Terminal

- Operate Terminal only through terminal tools; do not assume that plain assistant text is terminal input.
- Use the namespaced Terminal tool names for terminal operations: `terminal__terminal_exec`, `terminal__terminal_write_stdin`, and `terminal__terminal_terminate`.
- `terminal_exec` creates a new session when `session_id` is null and reuses an existing session only when `session_id` is a valid id returned by a prior `terminal_exec` or `terminal__get_state`. Never fabricate a session id such as `ts1`, `terminal-session-1`, or an empty string; send `null` when you want a new session.
- If a command is still running, continue with `terminal_write_stdin` and explicitly provide the target `session_id`. Send empty text when you only want to wait for more output.
- For `terminal_write_stdin`, omit `wait_mode` or use `any_output` to return after the next output update; use `timeout` to wait the full yield window or process exit without streaming intermediate progress updates.
- Never use interactive full-screen terminal programs such as vim, vi, nano, less, or top. Use non-interactive commands such as `cat`, `grep`, `head`, `tail`, or `python -c` to inspect files; prefer `apply_patch` for edits instead of shell string assembly.
- Never proactively start commands that require human accounts, passwords, browser authorization, device-code authorization, or interactive login wizards, such as `gh auth login`, `docker login`, or `npm login`. Prefer public webpages, HTTP APIs, `git clone`, `curl`, or unauthenticated lookup paths.
- If the terminal is already stuck in an authentication or login prompt you should not enter, do not continue answering wizard questions; interrupt it and switch to a non-interactive approach.
