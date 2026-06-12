# Contributing

Daat Locus is a runtime project, not a prompt collection. Contributions should
preserve the boundaries that make state explicit, actions auditable, and
experience reusable.

Read [Architecture](docs/architecture.md) first if you are changing runtime
objects, tools, Apps, workflows, memory, sleep, daemon behavior, or persistence.
Coding-agent-facing constraints live in [AGENTS.md](AGENTS.md).

## Design Rules

- Plain assistant text must not cause external side effects.
- Prefer explicit ids over hidden selected cursors.
- Keep `Event`, `PendingWork`, `App`, `Plan`, `Workflow`, `Memory`, and `Sleep`
  as separate concepts.
- Let code handle mechanical work such as lookup, deduplication, freshness
  checks, persistence, delivery bookkeeping, and evidence recording.
- Use App-scoped tools for stateful operating surfaces instead of expanding one
  flat global tool list.
- Keep runtime error correction separate from workflow improvement.
- Treat workflow changes as changes to reusable execution assets, not as prompt
  edits or chat memory.

## Quality Gates

CI currently runs:

```bash
cargo fmt --all -- --check
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

When adding or changing a tool:

- state what world state it reads or mutates
- require explicit identifiers where stale state can exist
- validate schemas at declaration and execution time
- decide whether the tool creates a turn boundary
- test invalid arguments and tool availability
- add runtime error evidence only when misuse is code-detectable

Do not rely on hidden UI state when a concrete id or freshness guard is
available.

## Adding Or Changing Apps

An App is a focusable, stateful operating surface.

When changing an App:

- keep `state`, `usage`, and `how_to_use` separate
- define focus and blur behavior when relevant
- keep dynamic tools scoped to the App
- keep app notices explicit and resolvable
- do not put reusable task procedures into App prompt docs

Transports such as Telegram are not Apps by default. If code already receives a
structured external fact, model it as an event source rather than an interface
the model must navigate.

## Changing Workflows Or Sleep

When changing workflow or sleep behavior:

- declare the evidence type consumed
- declare the persistent artifact produced
- keep runtime protocol correction and workflow process improvement separate
- do not feed raw full conversations into runtime error correction
- do not patch workflows from runtime protocol errors
- do not let sleep mutate builtin workflows

Runtime error correction changes global tool and protocol contracts. Workflow
improvement changes reusable execution processes for task classes.

## High-Risk Areas

Treat these areas as high risk:

- daemon authentication and lifecycle
- runtime turn scheduling, context compaction, and pending work
- event completion and Telegram delivery
- terminal process management
- browser reference freshness
- filesystem sandboxing
- workspace app workers
- provider credentials and OAuth storage
- Hindsight retain and recall integration
- sleep-time contract and workflow evolution

High-risk changes should be small, reviewable, and covered by targeted tests
where possible.
