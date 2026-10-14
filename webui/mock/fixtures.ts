/**
 * Boot fixtures for the mock daemon: a root Actor (approval mode "ask"), one
 * read-only child Actor ("Research", approval mode "none") and a few completed
 * historical exchanges with tool results so the timeline renders immediately.
 */
import type { JsonValue } from "../src/lib/actor-api";
import {
  addMessage,
  addTool,
  broadcastResync,
  configHolder,
  createActor,
  defaultConfig,
  emitEvent,
  newId,
  now,
  stageSettings,
  store,
  touchMessage,
  type ActorStore,
} from "./state";

type SeededTool = {
  name: string;
  summary: string;
  arguments: JsonValue;
  payload: JsonValue;
  model_content: string;
  outcome: JsonValue;
};

type Exchange = { user: string; reply: string; tool?: SeededTool };

export function resetStore(): void {
  store.actors.clear();
  store.order.length = 0;
  store.catalog.length = 0;
  store.catalogSeq = 0;
  store.approvalSeq = 0;
  store.attachments.clear();
  store.idempotency.clear();
  store.approvalWakers.clear();
  configHolder.current = defaultConfig();
  Object.assign(stageSettings, {
    label: "Mock Device",
    listen: "0.0.0.0:45826",
    interfaces: [],
    signaling_origin: null,
    stun: [],
    ssh: {},
    compute_priority: 100,
  });
  clearRuns();
  seed();
  broadcastResync();
}

/** Engine run registry hook (engine.ts registers its clearer to avoid cycles). */
let clearRuns: () => void = () => {};
export function registerRunClearer(clear: () => void): void {
  clearRuns = clear;
}

function seed(): void {
  const root = createActor({
    id: "root",
    name: "Root",
    parent_id: null,
    apps: ["actors"],
    model: "mock-large",
    approval_mode: "ask",
    read_only: false,
    readable_roots: ["~"],
    writable_roots: ["~"],
    message_targets: [],
    allow_children: true,
    workspace: "~",
    instructions: "You are the root Actor of the mock account.",
  });
  const research = createActor({
    id: "research",
    name: "Research",
    parent_id: "root",
    apps: ["files", "browser"],
    model: "mock-mini",
    approval_mode: "none",
    read_only: true,
    readable_roots: ["~/research"],
    writable_roots: [],
    message_targets: ["root"],
    allow_children: false,
    workspace: "~/research",
    instructions: "Read-only investigation Actor seeded by the mock server.",
  });

  seedHistory(root, [
    {
      user: "Welcome to the DaatLocus mock server. What can you do here?",
      reply:
        "This conversation is served entirely by the local mock daemon in `webui/mock`.\n\n" +
        "- Send a message and watch the streaming timeline, thinking view and a simulated **files__list_directory** tool round.\n" +
        "- This Actor uses approval mode `ask`, so the tool call pauses for your approval.\n" +
        "- Reset everything with `POST /__mock/reset`.\n",
      tool: {
        name: "files__list_directory",
        summary: "Listed 3 entries in .",
        arguments: { path: "." },
        payload: {
          path: ".",
          entries: [
            { name: "README.md", kind: "file", size_bytes: 4096 },
            { name: "src", kind: "directory", size_bytes: null },
            { name: "package.json", kind: "file", size_bytes: 1213 },
          ],
          next_cursor: null,
        },
        model_content: "README.md\nsrc/\npackage.json",
        outcome: { path: ".", entries: 3 },
      },
    },
    {
      user: "How do approvals work in the mock?",
      reply:
        "When an Actor is configured with approval mode `ask`, the engine pauses before a tool call and emits an `approval_requested` event. Resolve it from the approval card (or `POST /v1/approvals/{id}`) and the run continues; denying it finishes the tool round with an error instead.\n",
    },
  ]);

  seedHistory(research, [
    {
      user: "Summarize the notes in the research folder.",
      reply:
        "I scanned `~/research` (read-only) and found two notes:\n\n" +
        "1. `sse-protocol.md` — event cursor and replay rules.\n" +
        "2. `fixtures.md` — seed data checklist for the mock.\n\n" +
        "Both look up to date.\n",
      tool: {
        name: "files__list_directory",
        summary: "Listed 2 entries in ~/research",
        arguments: { path: "~/research" },
        payload: {
          path: "~/research",
          entries: [
            { name: "sse-protocol.md", kind: "file", size_bytes: 2210 },
            { name: "fixtures.md", kind: "file", size_bytes: 980 },
          ],
          next_cursor: null,
        },
        model_content: "sse-protocol.md\nfixtures.md",
        outcome: { path: "~/research", entries: 2 },
      },
    },
  ]);
}

function seedHistory(actor: ActorStore, exchanges: Exchange[]): void {
  const actorId = actor.summary.id;
  let ts = now() - exchanges.length * 3_600_000;
  for (const exchange of exchanges) {
    const user = addMessage(actor, {
      kind: "request",
      source: { kind: "user" },
      reply_to: { kind: "user" },
      content: exchange.user,
      status: "completed",
    });
    touchMessage(user, {
      created_at_ms: ts,
      started_at_ms: ts,
      finished_at_ms: ts,
      updated_at_ms: ts,
    });
    emitEvent(actorId, { type: "message_added", message_id: user.id });

    const replyStarted = ts + 1_000;
    const reply = addMessage(actor, {
      kind: "result",
      source: { kind: "actor", actor_id: actorId },
      reply_to: { kind: "user" },
      in_reply_to: user.id,
      content: "",
      status: "completed",
    });
    touchMessage(reply, {
      created_at_ms: replyStarted,
      started_at_ms: replyStarted,
      finished_at_ms: ts + 5_000,
      updated_at_ms: ts + 5_000,
      reply: exchange.reply,
      final_generation: 1,
    });
    emitEvent(actorId, { type: "message_added", message_id: reply.id });
    emitEvent(actorId, { type: "generation_started", message_id: reply.id, generation: 1 });

    if (exchange.tool) {
      const callId = newId();
      const startedAt = replyStarted + 500;
      const finishedAt = replyStarted + 1_500;
      emitEvent(actorId, {
        type: "tool_started",
        message_id: reply.id,
        call_id: callId,
        name: exchange.tool.name,
      });
      const resultId = newId();
      addTool(actor, {
        id: resultId,
        message_id: reply.id,
        call_id: callId,
        name: exchange.tool.name,
        summary: exchange.tool.summary,
        error: null,
        started_at_ms: startedAt,
        finished_at_ms: finishedAt,
        outcome: exchange.tool.outcome,
        actor_id: actorId,
        arguments: exchange.tool.arguments,
        payload: exchange.tool.payload,
        model_content: exchange.tool.model_content,
      });
      emitEvent(actorId, {
        type: "tool_finished",
        message_id: reply.id,
        call_id: callId,
        name: exchange.tool.name,
        result_id: resultId,
        summary: exchange.tool.summary,
        error: null,
        started_at_ms: startedAt,
        finished_at_ms: finishedAt,
        outcome: exchange.tool.outcome,
      });
    }

    emitEvent(actorId, { type: "message_updated", message_id: reply.id, status: "completed" });
    ts += 3_600_000;
  }
}
