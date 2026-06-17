---
description: Coding is the app to use when a task requires researching, reading, modifying, developing, or otherwise operating on a project. It includes all Terminal capabilities plus additional project-aware tools.
when_to_use:
  - When a task requires researching, reading, modifying, developing, or otherwise operating on any project.
  - When project work may need commands, tests, formatting, git, filesystem inspection, source-code navigation, semantic search, or edits; Coding includes all Terminal capabilities plus additional project-aware tools.
  - Project operations must use Coding app rather than Terminal app. Use Terminal directly only for non-project command execution or standalone process interaction.
---
Coding app is used to modify projects; think of it as a Coding Studio for the Agent.

First, if the project you need to edit is not open yet, use `coding__open_project`.

When editing source code, always prefer Coding app tools, such as `coding__search_code`, `coding__read_code`, and `coding__edit_code`, instead of substituting terminal commands. Important: except for configuration, generated assets, or other non-source areas outside SCOPE engine responsibility, or cases where these tools genuinely cannot complete the task, do not use other tools or shell commands to edit source code. When a Coding project scope is open, `apply_patch` is rejected for source files that SCOPE says it is responsible for; use `coding__edit_code` for those files.

After each edit, the tool automatically evaluates the impact of your changes and accumulates pending review events. You can read the current number of pending review events with `coding__get_state`. You do not need to handle them immediately. However, after you finish a series of edits (usually when a plan step is complete, or when you judge that too many review events have accumulated), call `coding__next_review` to acknowledge and claim review events; pass `limit` when many reviews are pending so several impact targets can be claimed in one response. Then follow their instructions to inspect the impact of your changes. This must always be done before reporting back to the user.

SCOPE engine configuration hints are returned by `coding__open_project` and are visible through `coding__get_state`, including available tree-sitter languages plus visible per-language `lsp_setup_hint` lines for LSP language/server setup guidance.

Coding app keeps app-level usage rules here. Search, read, and edit protocol details are owned by SCOPE and appended below from SCOPE's compiled usage interface.
