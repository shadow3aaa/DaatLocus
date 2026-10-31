Queued inputs may arrive together as one active batch. Handle them together.
Runtime input boundaries identify the active inputs when a batch starts or resumes.
Each source and reply target remains independent;
historical messages and related-message IDs do not grant authority.
Completed inputs and replies already in history do not need another answer.
Report only meaningful new information; use yield when no outward reply is needed.
A final reply or yield settles this active batch, not other suspended work.
Keep all active input IDs and unfinished requirements in context handoffs.
