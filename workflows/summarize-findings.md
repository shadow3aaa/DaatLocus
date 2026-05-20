---
id: summarize-findings
---

## When To Use
- An investigation, inspection, debugging session, or research task produced facts that need to be compressed for a user or downstream primitive.
- The task outcome is primarily understanding, not editing or committing.
- Multiple files, commands, or observations need to be organized into a concise result.
- The next step depends on distinguishing confirmed facts from assumptions and unknowns.

## Preconditions
- There are findings from local inspection, commands, code reading, browser research, or other tools.
- The user's original question or decision context is known.
- Important uncertainty, missing evidence, or failed checks have been noted.

## Workflow
1. Recenter the summary on the user's original question or decision.
2. Separate confirmed facts, reasoned interpretations, and unknowns or blockers.
3. Cite the most relevant evidence, such as files, symbols, commands, versions, pages, or error messages.
4. Omit low-value tool chatter and only include details that affect the answer or next action.
5. Provide recommended next steps when the findings imply a practical decision.
6. Hand off a concise summary artifact or send it through the final response path when the task is complete.

## Done Criteria
- The findings are concise, ordered by importance, and tied to the user's request.
- Evidence is specific enough to be actionable without reproducing all raw output.
- Uncertainty and blockers are clearly labeled.
- A downstream primitive or the user can decide what to do next from the summary.

## Recovery
- If evidence is contradictory, show the conflict and identify what would resolve it.
- If too much detail accumulates, group it by theme and preserve only the highest-impact examples.
- If a key fact is missing, either perform one targeted follow-up inspection or state the gap explicitly.
