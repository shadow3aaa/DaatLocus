# Contributing

Daat Locus is a runtime project, not a prompt collection. Contributions should
preserve the boundaries that make state explicit, actions auditable, and
experience reusable.

Read [Architecture](docs/architecture.md) first if you are changing runtime
objects, tools, Apps, workflows, memory, sleep, daemon behavior, sessions,
dashboard clients, or persistence. Coding-agent-facing constraints live in
[AGENTS.md](AGENTS.md).

## Design Rules

- Plain assistant text must not cause external side effects.
- Prefer explicit ids and freshness guards over hidden selected cursors.
- Keep `Event`, `PendingWork`, `App`, `Plan`, `Workflow`, `Memory`, `Sleep`,
  `Manager`, and `Session` as separate concepts.
- App tools are called directly through their namespace. Do not reintroduce app
  focus or activation as a tool-exposure gate.
- Telegram is a transport and event source, not an App.
- Static file tools are `read_file` and `edit_file`; they are runtime tools, not
  an App namespace.
- Let code handle mechanical work such as lookup, deduplication, freshness
  checks, persistence, delivery bookkeeping, schema validation, and evidence
  recording.
- Keep AfterClaim Context, PreTurn Context, capability docs, App state, and
  memory as separate layers.
- Keep runtime error correction separate from workflow improvement.
- Treat workflow changes as changes to reusable SOP skill assets, not as

## Quality Gates

CI currently runs:

```bash
cargo fmt --all -- --check
cd webui && bun install --frozen-lockfile && bun run test
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo deny --locked check bans sources licenses
```

Run the relevant subset locally before submitting changes. For high-risk
runtime changes, add focused tests or include a clear manual verification note.

## TUI Performance Command

`tui-perf-cmd` enables a hidden developer command for deterministic dashboard
render checks. It is a non-default feature and is not part of the normal CLI
surface.

Run the default mixed scenario:

```bash
cargo run --features tui-perf-cmd -- dev tui-perf \
  --scenario mixed --frames 120 --warmup 10 --width 120 --height 40
```

Emit JSON for scripts:

```bash
cargo run --features tui-perf-cmd -- dev tui-perf \
  --scenario long-history --frames 240 --warmup 20 --width 140 --height 48 --json
```

Available scenarios:

- `mixed`: committed activity, live cells, skills toggle panel, markdown, diffs,
  browser, terminal, and reply cells.
- `long-history`: many committed activity cells with an explicit scroll offset.
- `scrolling`: deterministic scroll movement through a large activity list.
- `live-activity`: active runtime status and live activity cells.
- `command-panels`: skills list panel and command-bar rendering.

The command renders through the same dashboard frame function used by the real
TUI, but it uses `ratatui::backend::TestBackend` instead of entering the
terminal alternate screen. Reported metrics include frame, prep, draw, activity,
and command timing, plus activity render-cache hit/miss counts.

Use this command when a TUI change might affect render cost or frame scheduling.
Treat absolute milliseconds as local-machine data; compare scenarios across
nearby revisions instead of relying on tight cross-machine thresholds.

## Commit Messages

Commit messages must be in English. Use a title that names the real behavior or
boundary being changed, for example:

- `Fix Telegram event completion retry`
- `Add workspace app notice polling tests`
- `Document workflow sleep evidence model`

Avoid vague titles such as `fix`, `update`, `misc`, `wip`, or `cleanup`.

## Adding Or Changing Tools

When adding or changing a model-facing tool:

- state what world state it reads or mutates
- require explicit identifiers where stale state can exist
- validate schemas at declaration and execution time
- use the conservative model-facing JSON Schema dialect
- do not create a turn stop reason outside `Finished`, `Error`, `Compacted`,
  or `Interrupt`
- test invalid arguments and tool availability
- add runtime error evidence only when misuse is code-detectable

Do not rely on hidden UI state when a concrete id or freshness guard is
available.

## Adding Or Changing Apps

An App is a stateful capability domain, not a focus gate.

When changing an App:

- keep `state` and `docs` separate
- expose model-facing tools through the App namespace
- keep the generated `appid__get_state` surface accurate and cheap
- keep app notices explicit and resolvable
- bind operations to explicit ids such as page ids, terminal session ids, paths,
  or app-specific object ids
- do not put reusable task procedures into App prompt docs
- do not add focus/blur requirements unless the runtime contract is explicitly
  redesigned

Transports such as Telegram are not Apps by default. If code already receives a
structured external fact, model it as an event source rather than an interface
the model must navigate.

Workspace Apps currently use one Lua 5.4 module loaded from `runtime/app.lua`.
The supported hook surface is `config`, `init`, `render_state`, `list_tools`,
`call_tool`, and `poll_notices`; do not document or depend on removed
`on_focus` / `on_blur` hooks.

## Changing Coding Or File Tools

Coding source operations use `path + line#hash` anchors:

- `coding__search_code` returns matched source lines.
- `coding__read_code` accepts a path plus anchor and `around` or `full` mode.
- `coding__edit_code` applies structured edits with SCOPE validation and review.
- `read_file` handles explicit file/range reads.
- `edit_file` handles non-SCOPE ordinary file edits.

When a Coding project is open, `edit_file` must not edit SCOPE-owned source
files. Use `coding__edit_code` so propagation review is available.

## Changing Manager, Sessions, Or Transports

The Manager is the only public server. Session processes are private runtime
workers reached through Manager-owned IPC.

When changing this area:

- keep public clients connected to the Manager, not direct Session endpoints
- keep runtime `Context`, `EventStore`, `PendingWorkQueue`, `Plan`, memory, apps,
  and model loop inside one Session
- keep the Manager responsible for public auth, session registry, lifecycle,
  routing, and Telegram default-session mapping
- do not expose IPC names, tokens, process ids, or lifecycle internals as normal
  WebUI/TUI/API state
- keep `daat-locus code <project-dir>` as a multi-session project selector, not
  a project-to-single-session mapping

## Changing Dashboard Clients

`DashboardState` is shared session/runtime state. `TuiViewState` is local to one
TUI client. Do not solve local TUI behavior by mutating shared dashboard state.

TUI rendering should stay a pure full-frame projection scheduled by
`FrameRequester`. Input handling should reduce input into local view-state
changes or explicit `DashboardAction` effects; it should not render directly.

WebUI session rendering should use structured dashboard and activity-cell data.
Do not parse rendered TUI strings, prose, or command output into web structure
when a typed contract should exist instead.

## Changing Workflows Or Sleep

When changing workflow or sleep behavior:

- declare the evidence type consumed
- declare the persistent artifact produced
- keep runtime protocol correction and workflow process improvement separate
- do not feed raw full conversations into runtime error correction
- do not patch workflows from runtime protocol errors
- do not let sleep mutate builtin workflows
- do not persist temporary skill compositions as new skill specs by

Runtime error correction changes global tool and protocol contracts. Workflow
improvement changes reusable SOP skill specs for task classes.

## High-Risk Areas

Treat these areas as high risk:

- daemon authentication and lifecycle
- Manager/Session IPC and session registry
- runtime turn scheduling, context compaction, and pending work
- event completion and Telegram delivery
- terminal process management
- browser reference freshness
- filesystem sandboxing
- Coding/SCOPE edit and propagation review boundaries
- workspace app worker lifecycle and schema validation
- provider credentials and OAuth storage
- Hindsight retain and recall integration
- sleep-time contract and workflow evolution

High-risk changes should be small, reviewable, and covered by targeted tests
where possible.
