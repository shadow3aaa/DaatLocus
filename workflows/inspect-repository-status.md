---
id: inspect-repository-status
---

## When To Use
- A task involves a local Git repository and the current branch, cleanliness, or upstream state matters.
- Before modifying, committing, pushing, rebasing, or reporting on repository changes.
- The user asks what changed, whether work is committed, or whether local and remote state are aligned.
- A later primitive needs a reliable repository-status artifact as input.

## Preconditions
- The repository path is known or can be discovered from the current project context.
- Local Git commands can be run non-interactively.
- The task does not require entering credentials or completing an interactive authentication flow.

## Workflow
1. Confirm the repository root and current branch.
2. Run a concise status check that includes branch/upstream information, such as `git status --short --branch`.
3. Inspect the current head or recent commit when commit, push, or comparison decisions may depend on it.
4. If the working tree has changes, classify them as intended task changes, pre-existing user changes, generated artifacts, or uncertain files.
5. Check upstream divergence only when the task may need pull, push, rebase, or a clear ahead/behind report.
6. Preserve a short status artifact for downstream work: root, branch, upstream, head, changed files, and blockers.

## Done Criteria
- The repository root, branch, upstream relationship, and working-tree cleanliness are known.
- Any modified, staged, untracked, or conflicted files are listed or intentionally scoped.
- Any push/pull/authentication blockers are identified before later primitives act on the repository.
- Downstream primitives can decide whether it is safe to edit, test, commit, or push.

## Recovery
- If the directory is not a Git repository, report that status and continue only with primitives that do not require Git.
- If Git reports conflicts or an in-progress operation, stop repository-changing actions and ask whether to continue recovery.
- If remote inspection requires credentials or network access that is unavailable, record local status and defer remote-dependent steps.
- If unrelated user changes are present, avoid staging, overwriting, or reverting them unless explicitly asked.
