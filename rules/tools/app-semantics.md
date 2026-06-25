# App Semantics Rules

## Current App Semantics

### Terminal

`Terminal` is the interface for local command execution and persistent process interaction.

It is an `App` not because the command line is important, but because:

- sessions persist
- output may need to be awaited
- stdin may need further writes
- unread output and process lifecycle need structured state

Operational constraints:

- Operate only through the Terminal app tools, exposed with the `terminal__`
  namespace.
- Do not treat interactive full-screen programs as the normal path.
- Do not hand interactive login or authentication flows to the model.
- Sessions are explicitly addressed; there is no hidden selected session.

### Browser

`Browser` is the interface for viewing and interacting with web pages.

It is an `App` because:

- page content is naturally local and time-dependent
- loading may need to be awaited
- interactions require reading a semantic snapshot before using `element_ref`
- later interactions depend on a persistent page session

Operational constraints:

- Operate only through browser tools.
- Interactions must explicitly provide `page_id + element_ref`.
- If a page change invalidates refs, reread the page instead of blindly retrying old refs.
- Search result pages are usually leads for locating sources, not final facts.

### Coding

`Coding` is the interface for semantic code operations powered by scope-engine.

It is an `App` because:

- project state (open project, LSP connections) persists across tool calls
- symbol lookups and propagation analysis require an open project context
- the model needs to see propagation status to decide whether to continue editing

**State rendering design:**

Coding must render its key state into `AppStateRender` so that:

1. **State is available on demand** — the model can call `coding__get_state` to
   read the current project, LSP status, and pending propagation events without
   relying on conversation history.
2. **Tool return values carry immediate feedback** — `search_code` returns
   path-scoped line-hash hits, `read_code` verifies a path plus line anchor and
   returns hash-anchored source lines, and `edit_code` returns propagation
   results so the model sees impact scope mid-turn.
3. **Notice is NOT for propagation review** — Coding uses `notice_reason()` only
   for background events like "LSP server crashed" or "project index ready".
   Propagation review is handled through tool return values and state rendering,
   not through notice-triggered turn interrupts.

Operational constraints:

- Coding tools (`coding__search_code`, `coding__read_code`,
  `coding__edit_code`, and review tools) are app-domain tools and must be
  called directly through the `coding__` namespace.
- Use `read_file` for explicit path/range reads. Do not make `read_code`
  support arbitrary path/range compatibility.
- Use `edit_file` for non-SCOPE files. When the current project scope is open,
  `edit_file` must reject SCOPE-owned source files and require
  `coding__edit_code`.
- Coding `render_state()` must include: project_root, open_languages,
  lsp_status, propagation_pending_count, and up to N recent propagation events.
- LSP process lifecycle (start, crash recovery, shutdown) belongs to Coding
  internals, not to tool return values.

### App Tool Domains

All app tools are model-facing through explicit app namespaces. Runtime tool
spec construction should expose every installed app's valid tool specs, plus the
generated `appid__get_state` tool for that app.

Examples:

- Coding tools: `coding__open_project`, `coding__search_code`,
  `coding__read_code`, `coding__edit_code`, `coding__next_review`
- Terminal tools: `terminal__terminal_exec`,
  `terminal__terminal_write_stdin`, `terminal__terminal_terminate`
- Browser tools: `browser__browser_open_page`, `browser__browser_snapshot`,
  `browser__browser_click`
- State tools: `coding__get_state`, `terminal__get_state`,
  `browser__get_state`

Static runtime file tools such as `read_file` and `edit_file` are not owned by
any app. Their availability is governed by runtime policy and the SCOPE
boundary, not by app tool domains.

Do not reintroduce app composition as a tool-exposure mechanism. Cross-domain
work should simply call the correct namespaced tool directly.
