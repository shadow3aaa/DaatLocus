# Commit History Rules

## Commit History

Commit history is a long-term engineering interface, not a temporary chat log. When rewriting history or adding commits, follow these rules:

- Commit messages must be in English. The title should use imperative mood or a clear action phrase, such as `Add ...`, `Fix ...`, `Refactor ...`, `Remove ...`, `Split ...`, or `Document ...`.
- The title must state the real subject and purpose of the change. Avoid uninformative titles such as `update`, `fix`, `u`, `misc`, `wip`, or `cleanup`.
- One commit should represent one logical concern. Split behavior changes, refactors, formatting, documentation, tests, and dependency updates unless they cannot compile or cannot be explained independently.
- Large refactor commits must name the boundary being split, such as `Split runtime turn scheduling modules`; do not write only `Refactor runtime`.
- Bug-fix commit titles should describe the fixed behavior rather than only the symptom, such as `Retry Telegram delivery instead of failing events`.
- Pure mechanical formatting should be its own commit, such as `Format Rust sources after refactor`.
- Do not commit local research directories, generated caches, runtime logs, or unconfirmed experiments.
- Before rewriting already-pushed history, create a local backup branch. Push rewritten `main` with `--force-with-lease`.
