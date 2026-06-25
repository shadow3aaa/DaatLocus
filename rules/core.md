# Core Agent Rules

This document defines the agent-facing boundaries that match the current Daat Locus implementation.

The goal is not to write abstract slogans. The goal is to give future changes to `app`, `events`, `runtime_tools`, `runtime_context`, `preturn_state`, `telegram_transport`, `workflow`, `memory`, `sleep`, and related modules a set of design constraints that match how the code actually works.

## Project Reality

- Daat Locus is a long-running, tool-driven agent.
- Its main loop is not a chat app where a user sends one message and the model sends one answer. It is a runtime where structured context is injected, the model decides what to do, and tools mutate the world.
- External input enters the current turn primarily through `Event`, `PendingWork`, background app notices, and automatic memory recall.
- Plain assistant text is normally an explanation or intermediate record inside the runtime. It is not automatically sent to Telegram or any other external system.
- Real-world changes must happen through explicit tool calls.

## Non-Negotiables

- Telegram is not an `App`; it is a transport and event source.
- Normal event completion must not be plain text only. It must explicitly call `finish_and_send`.
- Browser, Terminal, and Coding are built-in `App`s because they represent stateful capability domains with their own tools, state, and lifecycle.
- `App` and `Event` are parallel concepts. Do not collapse one into the other.
- Let the model make semantic judgments. Do not make the model perform mechanical enumeration, lookup, deduplication, or freshness checks that code can already perform.

## What Code Should Do

Code is responsible for:

- polling and receiving Telegram updates
- deduplicating events
- persisting state
- loading builtin primitive specs and workspace primitive specs
- writing `PrimitiveRunRecord` directly at the work-completion boundary
- claiming, releasing, and requeueing pending work
- maintaining the outbox
- loading structured runtime context
- enforcing tool availability policy and app namespace collisions
- recording traces
- running prompt compile and workflow evolution separately

Do not push these responsibilities onto the model.

In particular, do not make the model repeatedly perform:

- list
- select
- open
- read latest state
- dedupe
- freshness check
- delivery bookkeeping

## What The Model Should Do

The model is responsible for:

- understanding event semantics
- judging whether a response is needed
- choosing the right tool domain
- calling `appid__get_state` when current app state is needed
- judging whether to create or bind a workflow
- planning steps
- choosing tools
- calling `deep_recall` when needed
- producing the final `reply_message`

If a new interface mainly makes the model perform mechanical lookup, it is probably designed incorrectly.
