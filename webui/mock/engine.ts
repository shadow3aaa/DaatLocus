/**
 * Fake actor execution engine. A send starts a turn: after a short delay the
 * engine creates a running result message, streams reasoning + assistant
 * deltas, runs one simulated `files__list_directory` tool round (gated by an
 * actor mode is "ask"), then completes the turn and dequeues the next request.
 */
import type { ApprovalDecision, JsonValue, MessageRecord } from "../src/lib/actor-api";
import { registerRunClearer } from "./fixtures";
import {
  addApproval,
  addMessage,
  addTool,
  emitEvent,
  getActor,
  newId,
  now,
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
  wake: (() => void) | null;
};

const runsByUserMessage = new Map<string, Run>();
const runsByReply = new Map<string, Run>();
const activeActors = new Set<string>();

registerRunClearer(() => {
  for (const run of runsByUserMessage.values()) run.cancelled = true;
  runsByUserMessage.clear();
  runsByReply.clear();
  activeActors.clear();
});

export function isActorBusy(actorId: string): boolean {
  return activeActors.has(actorId);
}

/** Start (or queue) a turn for a freshly created user request message. */
export function submitTurn(actor: ActorStore, user: MessageRecord): void {
  if (user.status !== "running") return; // queued: drained by finishTurn
  activeActors.add(actor.summary.id);
  const run: Run = {
    actorId: actor.summary.id,
    userMessageId: user.id,
    replyId: null,
    cancelled: false,
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
    if (run.cancelled) run.wake?.();
  });
  return !run.cancelled;
}

function compose(actorName: string, input: string): { reasoning: string[]; assistant: string[] } {
  const topic = input.trim().replace(/\s+/g, " ").slice(0, 120) || "(an empty message)";
  return {
    reasoning: [
      `The Owner asked ${actorName}: "${topic}". `,
      "Check the workspace with the files__list_directory tool first, then answer with a short markdown summary.",
    ],
    assistant: [
      `You asked: **${topic}**\n\n`,
      "This reply is produced by the local development mock server. ",
      "It streams in small chunks so the live draft, ",
      "the thinking view and the tool round all render.\n\n",
      "### What the mock did\n",
      "1. Streamed a `reasoning` channel before this answer.\n",
      "2. Ran one simulated **files__list_directory** tool call with a fetchable result.\n",
      "3. Completed the message with this text as the final `reply`.\n\n",
      "> Tip: `POST /__mock/reset` re-seeds the fixtures at any time.\n",
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

    if (!(await rest(run, 300))) {
      cancelUserOnly(actor, user);
      return;
    }

    const reply = addMessage(actor, {
      kind: "result",
      source: { kind: "actor", actor_id: actorId },
      reply_to: { kind: "user" },
      in_reply_to: user.id,
      content: "",
      status: "running",
    });
    run.replyId = reply.id;
    runsByReply.set(reply.id, run);
    touchMessage(user, { status: "running" });
    emitEvent(actorId, { type: "message_added", message_id: reply.id });
    emitEvent(actorId, { type: "generation_started", message_id: reply.id, generation: 1 });

    const composed = compose(actorName, user.content);
    let assistantText = "";

    for (const chunk of composed.reasoning) {
      if (!(await rest(run, 30 + Math.floor(Math.random() * 20)))) return cancelRun(actor, user, reply, assistantText);
      emitEvent(actorId, { type: "generation_delta", message_id: reply.id, generation: 1, channel: "reasoning", text: chunk });
    }
    for (const chunk of composed.assistant) {
      if (!(await rest(run, 20 + Math.floor(Math.random() * 20)))) return cancelRun(actor, user, reply, assistantText);
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
        });
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
      ? "\nThe files__list_directory call was **denied**, so I skipped the listing.\n"
      : "\nThe listing above came from the simulated `files__list_directory` tool round.\n";
    for (const chunk of chunkAround(closing, 24)) {
      if (!(await rest(run, 20 + Math.floor(Math.random() * 20)))) return cancelRun(actor, user, reply, assistantText);
      assistantText += chunk;
      emitEvent(actorId, { type: "generation_delta", message_id: reply.id, generation: 1, channel: "assistant", text: chunk });
    }

    if (!(await rest(run, 50))) return cancelRun(actor, user, reply, assistantText);
    completeRun(actor, user, reply, assistantText);
  } finally {
    if (run.replyId) runsByReply.delete(run.replyId);
    runsByUserMessage.delete(run.userMessageId);
    activeActors.delete(actorId);
    drainQueue(actor);
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
  const finished = now();
  touchMessage(reply, {
    status: "completed",
    reply: text,
    final_generation: 1,
    finished_at_ms: finished,
  });
  emitEvent(actor.summary.id, { type: "message_updated", message_id: reply.id, status: "completed" });
  touchMessage(user, { status: "completed", finished_at_ms: finished });
  emitEvent(actor.summary.id, { type: "message_updated", message_id: user.id, status: "completed" });
}

function cancelRun(actor: ActorStore, user: MessageRecord, reply: MessageRecord, partial: string): void {
  touchMessage(reply, {
    status: "cancelled",
    cancel_requested: true,
    reply: partial || null,
    finished_at_ms: now(),
  });
  emitEvent(actor.summary.id, { type: "message_updated", message_id: reply.id, status: "cancelled" });
  touchMessage(user, { status: "completed", finished_at_ms: now() });
  emitEvent(actor.summary.id, { type: "message_updated", message_id: user.id, status: "completed" });
}

function cancelUserOnly(actor: ActorStore, user: MessageRecord): void {
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
