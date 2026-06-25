# Daat Locus Agent Rules

This directory contains agent-facing operational rules for Daat Locus. These files are execution contracts, not general product documentation.

Start with `../AGENTS.md` for the root summary and use this index to load the specific rule file for the area being changed.

## Rule index

- `core.md` — project reality, non-negotiables, and code/model responsibility split.
- `model-facing-schema.md` — portable model-facing JSON Schema dialect.
- `session-activity.md` — shared semantic session activity event contract.
- `ui/slash-commands.md` — slash command UX rules.
- `ui/webui-session.md` — WebUI session rendering rules.
- `ui/tui-dashboard.md` — TUI dashboard architecture and refactor boundaries.
- `architecture/multi-session.md` — manager/session architecture, registry, public API, persistence.
- `architecture/config-readiness.md` — manager boot and config readiness rules.
- `architecture/manager-session-ipc.md` — private Manager-to-Session IPC protocol.
- `runtime/model.md` — runtime turn model.
- `runtime/objects.md` — App, Event, PendingWork, Plan, Memory object boundaries.
- `runtime/workflow-and-sleep.md` — Workflow and Sleep/self-improvement boundaries.
- `runtime/context.md` — AfterClaim, PreTurn, historical metadata, and snapshot mapping rules.
- `tools/app-semantics.md` — Terminal, Browser, Coding, and app-domain tool semantics.
- `tools/file-and-coding-tools.md` — static file tools, Coding protocol, and SCOPE boundary.
- `tools/tool-design.md` — general, app, event, plan, and workflow tool design rules.
- `integrations/third-party-apps.md` — third-party app package format.
- `integrations/telegram.md` — Telegram transport/event semantics and resolution rules.
- `maintenance/commits.md` — commit history rules.
- `maintenance/anti-patterns.md` — anti-patterns to avoid.
- `maintenance/design-checklist.md` — interface design checklist and summary.
