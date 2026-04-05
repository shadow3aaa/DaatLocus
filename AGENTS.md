Telegram 不是设备，而是事件通道。
# Spinova Agent Guidelines

This document defines the core ACI boundary for Spinova. The goal is to keep agent-facing interfaces aligned with the actual strengths of LLMs and avoid pushing deterministic navigation work onto the model.

## Core Principle

- `Device` means an interactive viewport that requires active attention.
- `Event` means a newly arrived fact that enters the world passively and needs judgment.
- `Device` and `Event` are parallel concepts. An event may be produced by a device or by a transport, but it should not be collapsed into device viewport state.
- Do not model every external system as a `Device`.
- Do not force the model to perform deterministic navigation just to access facts that code already has.

## Device

Use a `Device` only when all of the following are true:

- The agent must focus it before meaningful operations should be allowed.
- The visible information is naturally partial and must be explored step by step.
- The interaction has temporal semantics such as waiting for output, stability, or continuation.

`Device` exists to answer:

- Where should attention go now?
- What interactive surface is currently visible?
- What operations are possible only when that surface is focused?

`Device` should carry device state, not agent state.

Allowed device state examples:

- focus / connectivity / health
- stable object lists and their metadata
- outbox or transport status
- concise usage hints for the device

Disallowed device state examples:

- agent cursors such as `selected_chat`, `selected_thread`, or `opened_message`
- hidden multi-step tool choreography state
- pending semantic judgments that are better represented as events

Typical example:

- `Terminal` is a `Device`.

## Event

Use an `Event` when the system has already received a structured external fact and the main task is semantic judgment rather than interface navigation.

`Event` exists to answer:

- What just happened?
- Does it require a response?
- What resolution should be applied to this specific occurrence?

Typical examples:

- A newly received Telegram message is an `Event`.
- A new obligation assignment is an `Event`.

## Device and Event Relationship

- A device may produce events.
- A transport may produce events without exposing a full interactive device workflow.
- Events explain why the agent should act.
- Devices constrain where attention must go before certain classes of actions are allowed.

In short:

- `Event` brings new facts into the prompt.
- `Device` gates the tool surface used to act on those facts.

## LLM vs Code

The LLM should do:

- semantic interpretation
- prioritization
- response planning
- resolution choice

Code should do:

- enumeration
- locating the target object
- fetching the latest state
- deduplication
- freshness checks
- deterministic execution

If the model is repeatedly doing `list`, `select`, `open`, `read`, or similar mechanical steps just to reach already-known facts, the abstraction boundary is probably wrong.

## Resolution Rules

- Resolution actions should bind to a concrete event, not only to a container such as `chat_id`, `thread_id`, or `page_id`.
- If freshness matters, the action contract must include an event identifier, message identifier, version, timestamp, hash, or another equivalent guard.
- If a newer event has arrived, older resolutions must be rejected or revalidated.

## Design Checklist

Before introducing a new agent-facing interface, check:

1. Is this thing primarily an interactive surface or a newly arrived fact?
2. Would a human describe the task as "go operate that interface" or as "something happened, decide what to do"?
3. Does the model need exploration, or does code already have the necessary facts?
4. If the model acts, is it acting on a specific event version?

If the answer is mostly about exploration and focus, prefer `Device`.

If the answer is mostly about receiving and resolving new facts, prefer `Event`.

## Tool Shape

- Device-scoped tools may require `focus_device(...)` first. This preserves attention discipline.
- After focus, a normal operation should usually complete in one explicit tool call.
- Prefer tools with explicit addressing such as `send(chat_id, message)` over hidden cursor-dependent flows.
- Avoid tool designs that require the model to mutate viewport state before a basic action becomes possible.
- `select_*` or `open_*` tools may exist for optional exploration, but they should not be the required path for routine handling of fresh events.

## Anti-Patterns

Avoid the following:

- exposing raw inbox navigation as the primary path for handling new messages
- requiring the model to manually open a target conversation before it can judge a fresh incoming message
- storing agent cursor state such as `selected_chat` inside device state
- making send / resolve depend on hidden viewport state rather than explicit identifiers
- binding resolve actions only to a chat or container id
- letting the model repeat deterministic UI navigation that code can perform safely

## Telegram Example

- `Telegram` as a chat UI can exist as a `Device`.
- A newly received Telegram message should first appear as a pending `Event`.
- The Telegram device viewport should show concise device information such as known chats and stable metadata, not agent cursor state.
- The default handling path should inject the pending message into context directly.
- A standard reply path should look like `focus_device(Telegram)` followed by one explicit action such as `telegram_send_message(chat_id, text)`.
- Reading the full chat history should be a secondary tool used only when extra context is actually needed.
- `selected_chat` is not an acceptable long-lived device state for the agent path.

In short:

- `Device` solves "where to look and how to operate".
- `Event` solves "what happened and whether to respond".
