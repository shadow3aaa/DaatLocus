# Telegram and Event Resolution Rules

## Current Event Semantics

### Telegram

In the current code, Telegram is:

- input side: `TelegramTransport` polls the Bot API and registers incoming events
- manager side: approved messages are routed through the per-chat default session mapping
- session side: each target Session owns its own Telegram event queue and outbound queue
- send side: completing an event enqueues a message into the session outbox; the Manager drains session outboxes and delivers them through the Bot API

Telegram is not an `App`.

Reasons:

- When a new message arrives, code already knows enough structured facts.
- Normal handling is "judge and respond", not "first navigate to a chat UI and explore".
- Standard runtime actions should bind `event_id` and explicit `chat_id`, not rely on hidden cursors.
- Session selection for Telegram is Manager routing state, not an agent-facing app cursor.

For approved Telegram chats, the Manager maintains a default session mapping:

- If `chat_id -> session_id` exists and the session still exists, ordinary messages route to that session.
- If no valid mapping exists, the first ordinary message creates a new `General` session, stores it as the chat default, and routes the event there.
- This automatic default is per Telegram chat. It is not a global main session.

Telegram-only session control commands are Manager-level commands. They must not
enter any session's `EventStore` as ordinary user events:

- `/session_list` lists sessions and marks the current chat attachment.
- `/session_new [title]` creates a new `General` session and attaches the current chat to it.
- `/session_attach <session_id_or_unique_prefix>` attaches the current chat to an existing session.
- `/session_delete <session_id_or_unique_prefix>` deletes the target session through Manager lifecycle deletion and removes Telegram default mappings that pointed to it.

These commands may accept a unique `session_id` prefix for Telegram usability.
If the prefix is ambiguous, code must ask for more characters instead of
guessing. `/session_attach` changes only the Manager's Telegram default mapping;
it does not inject an event into the target Session.

The standard path for an approved Telegram message:

1. The transport receives the message.
2. If the message is a Manager-level command, the Manager handles it directly and replies without creating a runtime event.
3. Otherwise, the Manager resolves or creates the chat's default session.
4. The Manager sends `EnqueueTelegramEvent` to the target Session over local IPC.
5. The Session registers the event in its own `EventStore`.
6. The Session enqueues `PendingWork::Event`.
7. The runtime claims the event.
8. The model judges and calls tools.
9. It ends the event with `finish_and_send`.
10. The Session writes the reply into its Telegram outbox.
11. The Manager drains the outbox, delivers through the Bot API, and records delivery back to the Session.

Unknown Telegram chats do not enter the normal event-processing path. They enter the ACL pending flow.

## Resolution Rules

All resolutions must bind to a specific event, not to a container.

Current minimum requirements:

- Operate on events through `event_id`.
- Disposition must be explicit: `resolved`, `dismissed`, or `failed`.
- `resolved` or `failed` must provide a non-empty `reply_message`.

Current event states include:

- `Pending`
- `Claimed`
- `AwaitingDelivery`
- `Resolved`
- `Dismissed`
- `Failed`

When designing a new event type, follow these principles:

- If stale/new event conflicts can exist in the world, actions must bind to a concrete version or equivalent freshness guard.
- Do not resolve only by container ids such as `chat_id`, `thread_id`, or `page_id`.
- Failure states should allow retry or revalidation rather than silently swallowing the problem.
