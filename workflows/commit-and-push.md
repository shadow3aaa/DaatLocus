---
id: commit-and-push
---

## When To Use
- The user explicitly asks to commit, push, publish, or otherwise persist local repository changes to a remote.
- A task's accepted completion criteria include a committed and pushed change.
- Repository status, validation results, and intended changed files are already known or can be checked immediately before committing.
- The operation is a Git commit/push boundary, not a general editing or validation primitive.

## Preconditions
- The repository status has been inspected and the intended files are identifiable.
- The user has granted permission to commit and push, either explicitly or through the task's stated goal.
- Required checks have passed, been intentionally skipped, or have documented blockers accepted by the user.
- A remote is configured, and the push path does not require an interactive authentication wizard.

## Workflow
1. Review the diff and changed-file list one final time for unrelated edits, generated noise, or secrets.
2. Stage only the files intended for this task.
3. Commit with an English message whose title clearly states the change in imperative mood or a direct action phrase.
4. Verify the new commit hash and working-tree state after committing.
5. Push to the intended remote and branch.
6. Capture the final artifact: commit hash, branch, remote push result, and any remaining local status.

## Done Criteria
- The intended changes are committed with a clear commit message.
- The commit was pushed to the intended remote branch, or the push blocker is documented.
- The final repository status is known and does not hide unintended staged or unstaged changes.
- The user can be told exactly what was committed and where it was pushed.

## Recovery
- If unrelated changes are present, do not stage them; ask for direction if they cannot be separated safely.
- If checks failed and the user did not accept the risk, return to modification or validation instead of committing.
- If commit creation fails, inspect status and resolve only the immediate repository issue needed to proceed.
- If push fails because of authentication, permissions, divergence, or network failure, do not start interactive login; report the blocker and safe next steps.
