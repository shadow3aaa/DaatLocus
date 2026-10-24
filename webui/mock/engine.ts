/**
 * Fake actor execution engine. A send starts a turn: after a short delay the
 * engine creates a running result message, streams reasoning + assistant
 * deltas, runs one simulated `files__list_directory` tool round (gated by an
 * actor mode is "ask"), then completes the turn and dequeues the next request.
 */
import type { ApprovalDecision, JsonValue, MessageRecord } from "../src/lib/actor-api";
import { registerRunClearer } from "./fixtures";
import { CODEX_IMPORT_MEDIA_TYPE } from "../src/lib/import-types";
import {
  addApproval,
  addTool,
  createActor,
  emitEvent,
  getActor,
  newId,
  now,
  ROOT_ACTOR_ID,
  resolveApprovalRecord,
  setActorStatus,
  store,
  touchMessage,
  type ActorStore,
} from "./state";

type Run = {
  actorId: string;
  userMessageId: string;
  replyId: string | null;
  cancelled: boolean;
  superseded: boolean;
  phase: "inference" | "tools";
  wake: (() => void) | null;
};

const runsByUserMessage = new Map<string, Run>();
const runsByReply = new Map<string, Run>();
const activeActors = new Set<string>();

registerRunClearer(() => {
  for (const run of runsByUserMessage.values()) {
    run.cancelled = true;
    run.wake?.();
  }
  runsByUserMessage.clear();
  runsByReply.clear();
  activeActors.clear();
});

export function isActorBusy(actorId: string): boolean {
  return activeActors.has(actorId);
}

/** Start (or queue) a turn for a freshly created user request message. */
export function submitTurn(actor: ActorStore, user: MessageRecord): void {
  if (user.status !== "running") {
    if (actor.summary.id === ROOT_ACTOR_ID && user.status === "queued") {
      const active = [...runsByUserMessage.values()].find(run => run.actorId === ROOT_ACTOR_ID);
      if (active) {
        active.superseded = true;
        if (active.phase === "inference") active.wake?.();
      }
    }
    return; // queued: drained after the current protocol group settles
  }
  activeActors.add(actor.summary.id);
  const run: Run = {
    actorId: actor.summary.id,
    userMessageId: user.id,
    replyId: null,
    cancelled: false,
    superseded: false,
    phase: "inference",
    wake: null,
  };
  runsByUserMessage.set(user.id, run);
  void execute(run, actor, user);
}

/**
 * Cancel the run behind a message id (either the user request or its running
 * reply). Returns true when a run was stopped.
 */
export function cancelTurn(messageId: string): boolean {
  const run = runsByUserMessage.get(messageId) ?? (messageId ? findRunByReply(messageId) : undefined);
  if (!run || run.cancelled) return false;
  run.cancelled = true;
  // Release a pending approval wait so the engine can observe the cancel.
  if (run.replyId) {
    const actor = getActor(run.actorId);
    for (const approval of actor?.approvals ?? [])
      if (approval.message_id === run.replyId && approval.decision === "pending")
        resolveApprovalRecord(approval.id, "cancelled", "Message cancelled");
  }
  run.wake?.();
  return true;
}

function findRunByReply(replyId: string): Run | undefined {
  return runsByReply.get(replyId);
}

/** Interruptible sleep; resolves early on cancel and reports it. */
async function rest(run: Run, ms: number): Promise<boolean> {
  await new Promise<void>(resolve => {
    const timer = setTimeout(() => {
      run.wake = null;
      resolve();
    }, ms);
    run.wake = () => {
      clearTimeout(timer);
      run.wake = null;
      resolve();
    };
    if (run.cancelled || run.superseded && run.phase === "inference") run.wake?.();
  });
  return !run.cancelled && !(run.superseded && run.phase === "inference");
}

function compose(actorName: string, input: string): { reasoning: string[]; assistant: string[] } {
  const topic = input.trim().replace(/\s+/g, " ").slice(0, 120);
  return {
    reasoning: [
      `${actorName} 需要处理：${topic}。`,
      "先查看相关文件，再核对修改后的布局。",
    ],
    assistant: [
      "我会先查看对话区域和输入框的布局。\n\n",
      "重点检查滚动范围、窄屏换行，以及输入框展开后历史消息的位置。\n\n",
    ],
  };
}

function chunkAround(text: string, size: number): string[] {
  const chunks: string[] = [];
  for (let index = 0; index < text.length; index += size) chunks.push(text.slice(index, index + size));
  return chunks.length ? chunks : [""];
}

async function execute(run: Run, actor: ActorStore, user: MessageRecord): Promise<void> {
  const actorId = run.actorId;
  const actorName = actor.summary.name;
  try {
    setActorStatus(actor, "running");

    if (!(await rest(run, actorId === ROOT_ACTOR_ID ? 1200 : 300))) {
      if (run.superseded && !run.cancelled) completeRun(actor, user, user, "");
      else cancelUserOnly(actor, user);
      return;
    }

    const reply = user;
    run.replyId = reply.id;
    runsByReply.set(reply.id, run);
    touchMessage(user, { status: "running" });
    emitEvent(actorId, { type: "message_added", message_id: reply.id });
    emitEvent(actorId, { type: "generation_started", message_id: reply.id, generation: 1 });

    if (actorId === ROOT_ACTOR_ID) {
      run.phase = "tools";
      const imported = user.attachments.find(item => item.media_type === CODEX_IMPORT_MEDIA_TYPE);
      if (imported) {
        if (!(await rest(run, 700))) return cancelRun(actor, user, reply, "");
        const id = newId(), config = actor.detail.config!;
        const importedActor = createActor({id, name: "Athena", parent_id: ROOT_ACTOR_ID, apps: ["actors", "files", "web", "browser_use"], model: config.model, approval_mode: config.permissions.approval_mode, readable_roots: config.permissions.readable_roots, writable_roots: config.permissions.writable_roots, workspace: config.workspace, instructions: imported.name});
        emitEvent(id, {type: "actor_changed", actor: importedActor.summary});
        const callId = newId(), resultId = newId(), outcome = {id, name: "Athena", conversation_url: `/actors/${id}`};
        addTool(actor, {id: resultId, actor_id: actorId, message_id: reply.id, call_id: callId, name: "actors__import_conversation", summary: "Imported conversation", error: null, model_content: "Actor prepared", arguments: {resource_id: imported.id, name: "Athena", instructions: imported.name}, outcome, payload: outcome});
        emitEvent(actorId, {type: "tool_finished", message_id: reply.id, call_id: callId, name: "actors__import_conversation", result_id: resultId, summary: "Imported conversation", error: null, outcome});
        completeRun(actor, user, reply, `已根据会话创建 [Athena](/actors/${id})，保留了原始上下文。它会等待你继续，尚未开始执行任务。`);
        return;
      }
      const callId = newId(), resultId = newId(), started = now();
      const text = "已经交给工作对话检查，完成后会把结果发到这里。";
      emitEvent(actorId, { type: "tool_started", message_id: reply.id, call_id: callId, name: "actors__send" });
      if (!(await rest(run, 250))) return cancelRun(actor, user, reply, "");
      addTool(actor, { id: resultId, actor_id: actorId, message_id: reply.id, call_id: callId, name: "actors__send", summary: "检查消息滚动与输入框布局", error: null, started_at_ms: started, finished_at_ms: now(), arguments: { actor_id: "research", content: user.content }, model_content: "请求已送达。", outcome: { actor_id: "research", name: "检查消息滚动与输入框布局" }, payload: null });
      emitEvent(actorId, { type: "tool_finished", message_id: reply.id, call_id: callId, name: "actors__send", result_id: resultId, summary: "检查消息滚动与输入框布局", error: null, outcome: { actor_id: "research", name: "检查消息滚动与输入框布局" } });
      completeRun(actor, user, reply, text);
      return;
    }

    const composed = compose(actorName, user.content);
    let assistantText = "";

    for (const chunk of composed.reasoning) {
      if (!(await rest(run, 40))) return cancelRun(actor, user, reply, assistantText);
      emitEvent(actorId, { type: "generation_delta", message_id: reply.id, generation: 1, channel: "reasoning", text: chunk });
    }
    for (const chunk of composed.assistant) {
      if (!(await rest(run, 30))) return cancelRun(actor, user, reply, assistantText);
      assistantText += chunk;
      emitEvent(actorId, { type: "generation_delta", message_id: reply.id, generation: 1, channel: "assistant", text: chunk });
    }

    // One simulated tool round, gated by an approval in "ask" mode.
    const mode = actor.detail.config?.permissions.approval_mode ?? "none";
    const callId = newId();
    let denied = false;
    if (mode === "ask") {
      const record = addApprovalSafe(actor, reply.id, callId);
      if (record) {
        emitEvent(actorId, {
          type: "approval_requested",
          approval_id: record.id,
          message_id: reply.id,
          call_id: callId,
          name: "files__list_directory",
        });
        const decision = await new Promise<ApprovalDecision>(resolve => {
          store.approvalWakers.set(record.id, resolve);
          run.wake = () => resolve("cancelled");
        });
        run.wake = null;
        if (run.cancelled) return cancelRun(actor, user, reply, assistantText);
        denied = decision !== "approved";
      }
    }

    const toolStarted = now();
    emitEvent(actorId, { type: "tool_started", message_id: reply.id, call_id: callId, name: "files__list_directory" });
    if (!(await rest(run, 250))) return cancelRun(actor, user, reply, assistantText);
    const toolFinished = now();

    const error = denied ? "Approval denied by Owner" : null;
    const resultId = newId();
    addTool(actor, {
      id: resultId,
      message_id: reply.id,
      call_id: callId,
      name: "files__list_directory",
      summary: denied ? "files list denied" : "Listed 3 entries in .",
      error,
      started_at_ms: toolStarted,
      finished_at_ms: toolFinished,
      outcome: denied ? null : { path: ".", entries: 3 },
      actor_id: actorId,
      arguments: { path: "." },
      payload: denied
        ? { denied: true }
        : {
            path: ".",
            entries: [
              { name: "README.md", kind: "file", size_bytes: 4096 },
              { name: "src", kind: "directory", size_bytes: null },
              { name: "package.json", kind: "file", size_bytes: 1213 },
            ],
            next_cursor: null,
          },
      model_content: denied ? "The Owner denied this files__list_directory call." : "README.md\nsrc/\npackage.json",
    });
    emitEvent(actorId, {
      type: "tool_finished",
      message_id: reply.id,
      call_id: callId,
      name: "files__list_directory",
      result_id: resultId,
      summary: denied ? "files list denied" : "Listed 3 entries in .",
      error,
      started_at_ms: toolStarted,
      finished_at_ms: toolFinished,
      outcome: denied ? null : { path: ".", entries: 3 },
    });

    const closing = denied
      ? "\n这次文件访问没有获批，任务已停止，之前的检查记录仍然保留。\n"
      : "\n已经找到相关文件，可以继续检查具体的布局规则。\n";
    for (const chunk of chunkAround(closing, 24)) {
      if (!(await rest(run, 30))) return cancelRun(actor, user, reply, assistantText);
      assistantText += chunk;
      emitEvent(actorId, { type: "generation_delta", message_id: reply.id, generation: 1, channel: "assistant", text: chunk });
    }

    if (!(await rest(run, 50))) return cancelRun(actor, user, reply, assistantText);
    completeRun(actor, user, reply, assistantText);
  } finally {
    if (store.actors.get(actorId) === actor) {
      if (run.replyId) runsByReply.delete(run.replyId);
      runsByUserMessage.delete(run.userMessageId);
      activeActors.delete(actorId);
      drainQueue(actor);
      if (!activeActors.has(actorId)) setActorStatus(actor, "idle");
    }
  }
}

function addApprovalSafe(actor: ActorStore, messageId: string, callId: string) {
  const permissions = actor.detail.config?.permissions;
  const call: { id: string; name: string; arguments: JsonValue } = { id: callId, name: "files__list_directory", arguments: { path: "." } };
  return addApproval(actor, {
    message_id: messageId,
    call,
    requested: {
      readable_roots: permissions?.readable_roots ?? [],
      writable_roots: permissions?.writable_roots ?? [],
      operation_details: { path: "." },
    },
    mode: "ask",
  });
}


function completeRun(actor: ActorStore, user: MessageRecord, reply: MessageRecord, text: string): void {
  if (store.actors.get(actor.summary.id) !== actor) return;
  const finished = now();
  touchMessage(reply, {
    status: "completed",
    reply: runsByUserMessage.get(user.id)?.superseded ? null : text,
    final_generation: 1,
    finished_at_ms: finished,
  });
  emitEvent(actor.summary.id, { type: "message_updated", message_id: reply.id, status: "completed" });
  if (user.id !== reply.id) {
    touchMessage(user, { status: "completed", finished_at_ms: finished });
    emitEvent(actor.summary.id, { type: "message_updated", message_id: user.id, status: "completed" });
  }
}

function cancelRun(actor: ActorStore, user: MessageRecord, reply: MessageRecord, partial: string): void {
  if (store.actors.get(actor.summary.id) !== actor) return;
  touchMessage(reply, {
    status: "cancelled",
    cancel_requested: true,
    reply: partial || null,
    finished_at_ms: now(),
  });
  emitEvent(actor.summary.id, { type: "message_updated", message_id: reply.id, status: "cancelled" });
  if (user.id !== reply.id) {
    touchMessage(user, { status: "completed", finished_at_ms: now() });
    emitEvent(actor.summary.id, { type: "message_updated", message_id: user.id, status: "completed" });
  }
}

function cancelUserOnly(actor: ActorStore, user: MessageRecord): void {
  if (store.actors.get(actor.summary.id) !== actor) return;
  touchMessage(user, { status: "cancelled", cancel_requested: true, finished_at_ms: now() });
  emitEvent(actor.summary.id, { type: "message_updated", message_id: user.id, status: "cancelled" });
}

/** Start the oldest queued request message, if any. */
function drainQueue(actor: ActorStore): void {
  const next = actor.messages
    .filter(message => message.status === "queued" && message.kind === "request")
    .sort((a, b) => a.seq - b.seq)[0];
  if (!next || activeActors.has(actor.summary.id)) return;
  touchMessage(next, { status: "running", started_at_ms: now() });
  emitEvent(actor.summary.id, { type: "message_updated", message_id: next.id, status: "running" });
  submitTurn(actor, next);
}
