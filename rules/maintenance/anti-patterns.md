# Anti-Patterns

## Anti-Patterns

Avoid these designs:

- Modeling transports such as Telegram, email, or notification centers as `App`s by default.
- Forcing the model to "open a chat" before handling a known new message.
- Storing transport cursors such as `selected_chat`, `selected_thread`, or
  `opened_message` in app state.
- Making send or resolve depend on hidden viewport state.
- Binding events only to container ids instead of event ids.
- Designing workflow as app-local supplemental instruction or innate model capability.
- Forcing the model to perform workflow semantic search before continuing.
- Auto-generating generic default workflow templates for blind model use.
- Persisting composite workflows for every recurring combination of primitives.
- Treating `when_to_use` text as the primary workflow runtime interface instead of exposing primitive capabilities and IO contracts.
- Showing only the first few lexicographically sorted workflow ids, or only filtered relevant ids, when task-time composition needs the full primitive ID vocabulary plus relevant details.
- Writing the current workflow binding back into the primitive spec itself.
- Making the model manually submit workflow result logs.
- Treating long-term memory as an immediate state cache.
- Treating plan as a backlog database.
- Letting the model implicitly submit final send actions through plain text.
