---
id: ask-clarifying-question
---

## When To Use
- The user's request is missing information that is necessary to act safely or correctly.
- Multiple reasonable interpretations would lead to materially different work.
- Local inspection cannot resolve the ambiguity, and assuming a default would risk wasted work, data loss, or an unwanted external action.
- The best next step is to ask the user a focused question rather than continue execution.

## Preconditions
- A concrete blocker or ambiguity has been identified.
- The missing information cannot be inferred reliably from current context, project files, or prior conversation.
- Any safe, low-cost inspection that could remove the ambiguity has already been considered.
- A user-facing reply channel or event completion path is available.

## Workflow
1. State the specific missing decision or fact that blocks safe progress.
2. Ask the smallest number of questions needed to unblock the task.
3. Offer clear options or a recommended default when that will reduce back-and-forth.
4. Avoid mixing the question with speculative implementation details that may change after the answer.
5. If an event is currently claimed, send the question through the required event completion tool rather than plain assistant text.

## Done Criteria
- The user receives a concise clarification request.
- The question identifies exactly what decision or information is needed.
- The request includes enough context or options for the user to answer efficiently.
- No irreversible or high-risk action was taken based on an unsupported assumption.

## Recovery
- If part of the task can be advanced safely without the missing answer, do only that limited inspection before asking.
- If the user later answers with incomplete information, ask one narrower follow-up instead of restarting the full task.
- If a reasonable default becomes clear during inspection, proceed with that default and mention the assumption in the final report.
