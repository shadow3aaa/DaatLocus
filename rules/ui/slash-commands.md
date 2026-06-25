# Slash Command Rules

## Slash Command Design

Slash commands in interactive clients are entry points, not miniature CLIs.
When a TUI user types a slash command, the preferred behavior is to open a
bottom interactive surface where the next step is selected, searched, toggled,
or inspected.

Rules:

- Prefer one memorable top-level slash command over a tree of user-visible
  subcommands.
- Use bottom panels for selection, search, toggles, and detail inspection.
- Do not require users to type object names, ids, or paths when code can present
  a picker.
- Query flows should be list/detail views, not `show <target>` commands.
- State refresh flows should normally be internal behavior, not visible
  `reload` commands.
- Keep action menus short and stable. For example, `/skills` should expose only
  high-level actions such as `List skills` and `Enable/Disable Skills`.
- Slash completion should show only commands that users should remember as
  product-level entry points.
- Internal verbs may exist for manager APIs, tests, remote command execution, or
  Telegram text control, but interactive TUI completion must not expose them as
  the primary UX.
- Normal successful flows should stay inside panels. Use short feedback only for
  errors, unavailable actions, invalid input, or fire-and-forget operations.

Examples:

```text
/skills     -> action menu -> skill list or enable/disable toggle view
/telegram   -> action menu -> status detail or pending request picker
/debug      -> action menu -> readonly detail views
/app-status -> app picker -> app detail view
/status     -> readonly detail view
```

If the user's next step is to choose, browse, toggle, or inspect something, it
belongs in a bottom panel rather than in a visible slash subcommand.
