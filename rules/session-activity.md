# Session Activity Event Rules

## Session Activity Event Protocol

Session activity is a shared semantic event stream. It is not a shared rendered
cell model, not a tool-specific UI protocol, and not a generic text log.

In this section, `Event` means a session activity event: a typed fact that the
session wants clients to render in the activity feed, transcript, or history.
This is separate from the external-input `Event` stored in `EventStore`, but the
same design rule applies: the payload describes what happened, not how a client
should draw it.

The public Manager/Session/dashboard contract should expose session activity as
typed events. Tools, model output, runtime status changes, and other
client-visible session activity all enter the same semantic event stream. The
TUI, WebUI, transcript overlay, history views, and future clients each derive
their own local view models from that event stream.

The desired flow is:

```text
runtime / tools / model
  -> session activity Event
  -> TUI derives a local render view model
  -> WebUI derives AgentChatActivityItem or another React-local view model
  -> transcript derives raw/copyable text
```

Rules:

- Activity payloads describe the data itself, not how that data should be
  displayed. Do not store titles, preview lines, expanded/collapsed state,
  detail indentation, UI hints, markdown truncation, colors, icons, bullets, or
  layout markers in the shared activity contract.
- Do not force every activity into a generic payload. Preserve the natural
  structure of each domain event: terminal executions have commands, status,
  cwd, stdout/stderr, exit code, and timing; patches have files and diff lines;
  plans have steps; browser snapshots have URLs and captured content; model
  reasoning has source text.
- If an event's source data is text, keep it as source text. For example,
  thinking/reasoning should be `Thinking { content: String }`, not
  `title/body_lines/full_body/expanded`.
- Labels such as `Thinking`, `Ran`, `Updated Plan`, bullet markers, glyphs,
  folding affordances, preview limits, and markdown rendering choices belong to
  clients.
- Local interaction state belongs to clients. Expanded thinking, scroll
  offsets, selected rows, active detail panes, copied transcript selection, and
  similar state must not be stored in the shared activity event.
- Tool execution must not emit a tool-specific UI protocol. It should emit
  semantic activity events or domain tool observations; renderers then decide
  how to display them.
- TUI render cells are local view models derived from activity events. They must
  not be the Manager/Session/WebUI protocol, persistence format, or history
  source of truth.
- WebUI-local view models, such as `AgentChatActivityItem`, are derived from
  activity events. They must not be produced by parsing TUI cells or stored as
  the source of truth for history.
- Raw text fallback is acceptable only for unknown/debug events or genuinely
  unstructured output. A supported activity type should have a typed semantic
  payload.
- Do not add compatibility layers that normalize old tool-UI, TUI-cell, or
  WebUI item contracts back into shared activity data. Delete or replace old
  render-shaped contracts when touching those paths.

Examples:

```text
Thinking { content }
AssistantMessage { content }
UserInput { content, attachments }
TerminalExecution { command, cwd, status, stdout, stderr, exit_code, duration }
BrowserSnapshot { url, content, line_count, ref_count }
WebSearch { query, url, summary }
PlanUpdated { kind, explanation, steps }
PatchApplied { files }
PrimitiveActivated { primitive_id }
ReplySent { disposition, subject, message }
```
