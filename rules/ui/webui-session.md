# WebUI Session Rendering Rules

## WebUI Session Rendering

These rules apply to session conversation and activity rendering only: the
Agent session message list, live activity, transcript-like views, and slash
command surfaces inside the session. Other WebUI pages such as navigation,
login, settings, logs, global status, and sidebars are normal web product
surfaces and do not need to mirror the TUI.

The WebUI session surface should stay close to the TUI in information hierarchy,
density, naming, and runtime semantics, but it must not consume TUI rendered
structures as its data source. WebUI should derive web-native markup and
interaction from the shared session activity event payloads.

Rules:

- Render from structured semantic activity events in `DashboardState`. Do not
  parse rendered TUI strings, bullet lists, status prose, command output, or
  legacy cells back into web structure.
- If WebUI needs structure that does not exist yet, add it to the
  Manager/Session/dashboard activity event contract. Do not add frontend
  regexes or string split heuristics as the normal path.
- Slash command output and interaction inside the session follow the same rule:
  use web-native controls, lists, toggles, buttons, and detail surfaces backed
  by semantic data. Plain preformatted output is only acceptable for explicit
  debug/raw views or unknown fallback content.
- Keep visual parity with the TUI through spacing, ordering, labels, status
  semantics, and progressive disclosure derived from the same semantic events.
  Do not copy TUI implementation structures such as render cells, wrapped
  lines, or cached render output.
- The old per-kind TUI glyph icon system is deprecated for product UI. Do not
  revive glyphs such as app-specific symbols for Browser, Coding, Patch, or
  Primitive. The standard current TUI layout markers such as `•`, `›`, and
  detail indentation are layout markers, not a per-kind icon system; mirror
  them only when the current TUI renderer uses them.
- Avoid nested cards in the session message list. Use flat activity rows,
  sections, lists, tables, collapsibles, separators, and inline controls.
