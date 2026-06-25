# Daat Locus Agent Guidelines

This file is the compact entry point for agent-facing rules in this repository. The detailed operational contracts live under `rules/`; they are rules, not general docs.

## Project Reality

- Daat Locus is a long-running, tool-driven agent.
- Its main loop is not a chat app where a user sends one message and the model sends one answer. It is a runtime where structured context is injected, the model decides what to do, and tools mutate the world.
- External input enters the current turn primarily through `Event`, `PendingWork`, background app notices, and automatic memory recall.
- Plain assistant text is normally an explanation or intermediate record inside the runtime. It is not automatically sent to Telegram or any other external system.
- Real-world changes must happen through explicit tool calls.

## Non-Negotiables

- Telegram is not an `App`; it is a transport and event source.
- Normal event completion must not be plain text only. It must explicitly call `finish_and_send`.
- Browser, Terminal, and Coding are built-in `App`s because they represent stateful capability domains with their own tools, state, and lifecycle.
- `App` and `Event` are parallel concepts. Do not collapse one into the other.
- Let the model make semantic judgments. Do not make the model perform mechanical enumeration, lookup, deduplication, or freshness checks that code can already perform.

## Rule Index

Read the specific rule file before changing the matching subsystem:

- Model-facing tool/input/output schemas: `rules/model-facing-schema.md`
- Slash commands: `rules/ui/slash-commands.md`
- Session activity event protocol: `rules/session-activity.md`
- WebUI session rendering: `rules/ui/webui-session.md`
- TUI dashboard architecture: `rules/ui/tui-dashboard.md`
- Multi-session architecture, public API, registry, persistence: `rules/architecture/multi-session.md`
- Manager boot and config readiness: `rules/architecture/config-readiness.md`
- Manager-Session IPC: `rules/architecture/manager-session-ipc.md`
- Runtime turn model: `rules/runtime/model.md`
- Core runtime objects: `rules/runtime/objects.md`
- Workflow and sleep/self-improvement: `rules/runtime/workflow-and-sleep.md`
- Runtime context split: `rules/runtime/context.md`
- App semantics: `rules/tools/app-semantics.md`
- Static file tools, Coding protocol, and SCOPE boundary: `rules/tools/file-and-coding-tools.md`
- Tool design rules: `rules/tools/tool-design.md`
- Third-party app packages: `rules/integrations/third-party-apps.md`
- Telegram and event resolution: `rules/integrations/telegram.md`
- Commit messages/history: `rules/maintenance/commits.md`
- Anti-patterns and interface checklist: `rules/maintenance/anti-patterns.md`, `rules/maintenance/design-checklist.md`

## Core Boundaries

- `App` owns a namespaced tool domain, structured state, lifecycle hooks, and client-visible status.
- `Event` decides what happened, whether to respond, and how to complete it.
- `PendingWork` decides what should drive the next turn.
- `Workflow` supplies SOP primitives that runtime can compose for the current task and sleep can keep corrected.
- `Plan` decides how the current task continues.
- `Memory` provides thread continuity and long-term experience.
- `Sleep` improves behavior from runtime mistakes.

When modifying these boundaries, use the code's real runtime behavior as the source of truth. Do not merge distinct concepts just for superficial uniformity.

## Commit History

Commit history is a long-term engineering interface, not a temporary chat log. Commit messages must be in English, use an informative imperative title, and represent one logical concern. See `rules/maintenance/commits.md` for the full rules.
