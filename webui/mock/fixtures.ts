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
  ROOT_ACTOR_ID,
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
  store.approvalWakers.clear();
  configHolder.current = defaultConfig();
  Object.assign(stageSettings, {
    label: "Mock Device",
    listen: "0.0.0.0:45826",
    interfaces: [],
    relay_origin: null,
    stun: [],
    ssh: {},
    compute_priority: 100,
  });
  clearRuns();
  seed();
  broadcastResync();
}

export function resetAccountWork(): void {
  clearRuns();
  store.actors.clear(); store.order.length = 0; store.catalog.length = 0;
  store.attachments.clear(); store.idempotency.clear(); store.approvalWakers.clear();
  workReset.generation = {revision: (workReset.generation?.revision ?? 0) + 1, writer: STAGE_ID};
  createActor({id: ROOT_ACTOR_ID, name: "Daat Locus", parent_id: null, apps: ["actors"], model: null,
    approval_mode: "auto", readable_roots: ["~"], writable_roots: ["~"],
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
    apps: ["actors"],
    model: "mock-large",
    approval_mode: "ask",
    readable_roots: ["~"],
    writable_roots: ["~"],
    workspace: "~",
    instructions: "You are the root Actor of the mock account.",
  });
  const research = createActor({
    id: "research",
    name: "检查消息滚动与输入框布局",
    parent_id: ROOT_ACTOR_ID,
    apps: ["files", "web", "browser_use"],
    model: "mock-mini",
    approval_mode: "none",
    readable_roots: ["~/research"],
    writable_roots: [],
    workspace: "~/research",
    instructions: "Investigation Actor seeded by the mock server.",
  });

  seedHistory(root, [
    {
      user: "帮我检查一下消息滚动和输入框布局。",
      reply:
        "已经交给工作对话检查，完成后会把结果发到这里。",
      tool: {
        name: "actors__send",
        summary: "检查消息滚动与输入框布局",
        arguments: { actor_id: "research", content: "检查消息滚动与输入框布局。" },
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
        outcome: { actor_id: "research", name: "检查消息滚动与输入框布局" },
      },
    },
    {
      user: "手机上的长消息也一起看看，输入框不要挡住最后一句。",
      reply:
        "会一起检查。重点看窄屏换行、输入框展开后的留白，以及历史消息的位置是否保持稳定。",
    },
  ]);

  seedHistory(research, [
    {
      user: "检查消息滚动与输入框布局，包含桌面和手机。",
      reply:
        "消息区现在铺满整个对话区域，输入框悬浮在底部。\n\n" +
        "- 展开输入框时，正在阅读的历史消息位置保持不变。\n" +
        "- 滚动到底部后，最后一句始终留在输入框上方。\n" +
        "- 390px 窄屏下，代码、表格和长链接可以独立横向滚动。\n\n" +
        "桌面和手机布局都已检查。",
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
    const reply = user;
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
