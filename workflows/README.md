# Workflows

This directory stores Daat Locus builtin workflow specification assets.

Rules:

- One markdown file per workflow.
- The file name is the workflow id.
- Frontmatter only contains `id`.
- These workflows are compiled into the program by `build.rs` and belong to builtin baseline capabilities.
- Builtin workflows are read-only and are never written back by `create_workflow`, sleep patch, or sleep merge.
- Runtime-evolvable workflows only live under `~/daat-locus-workspace/workflows`.

To add a builtin workflow later, add the corresponding `*.md` file directly under this directory.
