/**
 * Boot fixtures with one account assistant, a hosted Actor and completed
 * exchanges. Hosted requests originate from Actors, just like the real service.
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
  ROOT_ACTOR_ID,
  requestOrigin,
  stageSettings,
  store,
  touchMessage,
  workReset,
  STAGE_ID,
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
  workReset.generation = null;
  store.actors.clear();
  store.order.length = 0;
  store.catalog.length = 0;
  store.catalogSeq = 0;
  store.approvalSeq = 0;
  store.attachments.clear();
  store.idempotency.clear();
  configHolder.current = defaultConfig();
  Object.assign(stageSettings, {
    label: "Workstation",
    mode: "local",
    relay_origin: null,
    compute_priority: 100,
  });
  clearRuns();
  seed();
  broadcastResync();
}

export function resetAccountWork(): void {
  clearRuns();
  store.actors.clear(); store.order.length = 0; store.catalog.length = 0;
  store.attachments.clear(); store.idempotency.clear();
  workReset.generation = {revision: (workReset.generation?.revision ?? 0) + 1, writer: STAGE_ID};
  createActor({id: ROOT_ACTOR_ID, name: "Daat Locus", parent_id: null, apps: ["actors","admin_actors"], model: null,
    workspace: "~", instructions: ""});
  broadcastResync();
}

/** Engine run registry hook (engine.ts registers its clearer to avoid cycles). */
let clearRuns: () => void = () => {};
export function registerRunClearer(clear: () => void): void {
  clearRuns = clear;
}

function seed(): void {
  const root = createActor({
    id: ROOT_ACTOR_ID,
    name: "Daat Locus",
    parent_id: null,
    apps: ["actors","admin_actors"],
    model: "mock-large",
    workspace: "~",
    instructions: "You are the root Actor of the mock account.",
  });
  const research = createActor({
    id: "research",
    name: "Metis",
    parent_id: ROOT_ACTOR_ID,
    apps: ["files", "web", "browser_use"],
    model: "mock-mini",
    workspace: "~/research",
    instructions: "Investigation Actor seeded by the mock server.",
  });

  seedHistory(root, [
    {
      user: "Please check message scrolling and the composer layout.",
      reply:
        "Metis is checking it. I will bring the results back here.",
      tool: {
        name: "actors__message",
        summary: "Message sent to Metis",
        arguments: { actor_id: "research", content: "Check message scrolling and the composer layout." },
        payload: {
          message_id: "layout-request",
          sent_message: { conversation_url: "/actors/research", content: "Check message scrolling and the composer layout." },
        },
        model_content: "Message delivered to Metis.",
        outcome: { actor_id: "research", name: "Metis" },
      },
    },
    {
      user: "Check long messages on mobile too. The composer should not cover the last line.",
      reply:
        "I will cover narrow-screen wrapping, spacing around the expanded composer and the stability of the reading position.",
    },
  ]);

  seedHistory(research, [
    {
      user: "Check message scrolling and the composer on desktop and mobile.",
      reply:
        "Messages now fill the conversation area, with the composer floating at the bottom.\n\n" +
        "- Expanding the composer preserves the reading position.\n" +
        "- At the bottom, the last line stays above the composer.\n" +
        "- At 390px, code, tables and long links scroll horizontally on their own.\n\n" +
        "Checked both desktop and mobile layouts.",
      tool: {
        name: "files__list_directory",
        summary: "Listed 2 entries in webui/src",
        arguments: { path: "webui/src" },
        payload: {
          path: "webui/src",
          entries: [
            { name: "App.tsx", kind: "file", size_bytes: 28412 },
            { name: "index.css", kind: "file", size_bytes: 94816 },
          ],
          next_cursor: null,
        },
        model_content: "App.tsx\nindex.css",
        outcome: { path: "webui/src", entries: 2 },
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
      ...requestOrigin(actor),
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
    const reply = user;
    touchMessage(reply, {
      started_at_ms: replyStarted,
      finished_at_ms: ts + 5_000,
      updated_at_ms: ts + 5_000,
      reply: exchange.reply,
      final_generation: 1,
    });
    emitEvent(actorId, { type: "generation_started", message_id: reply.id, generation: 1 });

    if (exchange.tool) {
      const callId = newId();
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
        outcome: exchange.tool.outcome,
      });
    }

    emitEvent(actorId, { type: "message_updated", message_id: reply.id, status: "completed" });
    ts += 3_600_000;
  }
}
