---
id: report-result
---

## When To Use
- The task is complete or blocked and the user needs a final concise report.
- Work performed through tools must be translated into a user-facing outcome, including changes made, checks run, and caveats.
- A claimed event requires explicit completion through the runtime's final delivery tool.
- Downstream action is no longer needed until the user replies.

## Preconditions
- The actual work outcome is known: completed, partially completed, blocked, or failed.
- Important artifacts are available, such as changed files, commands run, check results, commit hashes, or unresolved blockers.
- Any required app notices or coding review events have been handled before reporting.
- The correct event completion path is available when an external event is claimed.

## Workflow
1. Summarize the concrete outcome in the first sentence.
2. List the most important changes, findings, or artifacts, keeping the report short and user-oriented.
3. Include validation commands and results when checks were run; state clearly if checks were skipped or blocked.
4. Mention remaining risks, assumptions, or next steps only when they matter.
5. If an event is claimed, call the required final delivery tool with the reply message instead of relying on plain assistant text.

## Done Criteria
- The user receives a final answer through the correct delivery mechanism.
- The report accurately reflects what was done and what was verified.
- Any blockers or limitations are explicit rather than hidden.
- The current task has no unresolved action left for the assistant.

## Recovery
- If final evidence is incomplete, run one targeted status or check command before reporting if practical.
- If a required delivery tool fails, retry only after inspecting the failure and avoid duplicate plain-text reports.
- If the task failed, use a failed disposition with a useful explanation and next step rather than pretending success.
