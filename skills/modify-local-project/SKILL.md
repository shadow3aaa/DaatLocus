---
name: modify-local-project
description: Apply minimal coherent changes to a local project using semantic coding tools.
---

## When To Use
- The task requires applying changes to a local project, such as source code, configuration, documentation, tests, or workflow assets.
- The desired outcome is clear enough to implement without first asking a blocking clarification.
- The work should preserve project conventions and avoid unrelated modifications.
- A later step will validate, commit, or report the changes.

## Preconditions
- The target project and relevant files have been inspected or are already known.
- The requested change and acceptance criteria are understood.
- Editing tools are available and appropriate for the file types involved.
- The repository status is known when unrelated user changes could be affected.

## Workflow
1. Define the minimal coherent change set that satisfies the user's request.
2. Edit source files with semantic coding tools when available, and use raw patching only for non-source or unsupported files.
3. Preserve existing style, naming, architecture boundaries, and documented project instructions.
4. Update nearby tests, documentation, configuration, or examples when the change makes them stale.
5. Review the resulting diff for accidental edits, generated noise, secrets, or unrelated file changes.
6. Inspect any propagation or impact-review events produced by the coding surface before considering the edit complete.
7. Hand off the changed-file list and validation needs to the next step.

## Done Criteria
- The requested local project changes are present in the intended files.
- The diff is limited to the task scope or any extra changes are explicitly justified.
- Style and project instructions were followed.
- Impacted areas have been reviewed enough to choose appropriate validation checks.

## Recovery
- If the implementation path becomes unclear, pause and inspect the relevant project area before continuing edits.
- If an edit tool rejects a patch, reread the current target and retry with a smaller, exact change.
- If unrelated user changes conflict with the requested edit, avoid overwriting them and ask for direction when necessary.
- If the change grows beyond the original scope, stop at a coherent boundary and report the additional work separately.
