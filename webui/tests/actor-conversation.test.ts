import { describe, expect, test } from "bun:test";
import { conversationReducer, initialConversation, queueAttachmentUploads, sameAccessSession, submissionFor } from "../src/hooks/use-actor-conversation";
import type { ActorDetail, ActorEvent, Attachment, EventRecord, MessageRecord } from "../src/lib/actor-api";
import { partitionConversationMessages } from "../src/lib/conversation-messages";
const event = (seq: number, value: ActorEvent, actor = "root"): EventRecord => ({ seq, actor_id: actor, created_at_ms: seq, event: value });
const message = (id: string, seq: number, status: MessageRecord["status"] = "queued"): MessageRecord => ({ id, seq, actor_id: "root", kind: "request", source: { kind: "user", }, reply_to: { kind: "user", }, in_reply_to: null, content: id, attachments: [], status, cancel_requested: false, reply: null, error: null, created_at_ms: 1, updated_at_ms: 1 });
describe("conversation semantic reducer", () => {
  test("the initial page restores an older active input without a queue request", () => {
    const active = message("active", 1, "running");
    let state = conversationReducer(initialConversation("root"), {type:"page",initial:true,page:{items:[message("latest", 100)],active,next_cursor:100,event_cursor:7}});
    expect(state.messages.map(item=>item.id)).toEqual(["active","latest"]);
    state = conversationReducer(state,{type:"message",message:{...active,updated_at_ms:2,status:"completed"}});
    state = conversationReducer(state,{type:"page",page:{items:[message("older", 99)],active,next_cursor:99}});
    expect(state.messages.find(item=>item.id === "active")?.status).toBe("completed");
    expect(state.cursor).toBe(7);
  });
  test("all incoming messages stay in chronological conversation order before and after receipt", () => {
    const inputs = [message("active", 1, "running"), message("owner", 2),
      {...message("actor", 3),source:{kind:"actor" as const,actor_id:"core"}},
      {...message("result", 4),kind:"result" as const},
      {...message("progress", 5),kind:"progress" as const}];
    let state = conversationReducer(initialConversation("root"), {type:"page",page:{items:inputs,next_cursor:null}});
    for (const primary of [false,true]) expect(partitionConversationMessages(state.messages,primary).transcript.map(item=>item.id)).toEqual(["active","owner","actor","result","progress"]);
    state = conversationReducer(state,{type:"event",record:event(10,{type:"message_updated",message_id:"actor",status:"running"})});
    state = conversationReducer(state,{type:"page",page:{items:[inputs[2]],next_cursor:null}});
    expect(state.messages.find(item=>item.id === "actor")?.status).toBe("running");
    expect(partitionConversationMessages(state.messages,false).transcript.map(item=>item.id)).toEqual(["active","owner","actor","result","progress"]);
  });
  test("session markers identify the exact account login", () => {
    expect(sameAccessSession("owner:selected", {kind:"owner",session_id:"selected",manage:true})).toBe(true);
    expect(sameAccessSession("owner:selected", {kind:"owner",session_id:"other",manage:true})).toBe(false);
  });
  test("paging orders and deduplicates messages, without overwriting live terminal status", () => {
    let state = initialConversation("root");
    state = conversationReducer(state, { type: "page", initial: true, page: { items: [message("new", 3)], next_cursor: 3, event_cursor: 10 } });
    state = conversationReducer(state, { type: "message", message: message("new", 3, "completed") });
    state = conversationReducer(state, { type: "page", page: { items: [message("old", 1), message("new", 3)], next_cursor: null } });
    expect(state.messages.map(item => item.id)).toEqual(["old", "new"]);
    expect(state.messages[1].status).toBe("completed");
    expect(state.cursor).toBe(10);
  });
  test("generation resets, stale deltas and replay preserve isolated source text", () => {
    let state = initialConversation("root");
    state = conversationReducer(state, { type: "event", record: event(1, { type: "generation_started", message_id: "m", generation: 1 }) });
    state = conversationReducer(state, { type: "event", record: event(2, { type: "generation_delta", message_id: "m", generation: 1, channel: "assistant", text: "old" }) });
    state = conversationReducer(state, { type: "event", record: event(3, { type: "generation_started", message_id: "m", generation: 2 }) });
    state = conversationReducer(state, { type: "event", record: event(4, { type: "generation_delta", message_id: "m", generation: 1, channel: "assistant", text: "stale" }) });
    const latest = event(5, { type: "generation_delta", message_id: "m", generation: 2, channel: "reasoning", text: "source" });
    state = conversationReducer(state, { type: "event", record: latest });
    const replay = conversationReducer(state, { type: "event", record: latest });
    expect(replay).toBe(state);
    expect(state.generations.m).toEqual({ generation: 2, assistant: "", reasoning: "source" });
    expect(conversationReducer(state, { type: "event", record: event(6, { type: "message_added", message_id: "other-input" }, "other") })).toBe(state);
  });
  test("tool events retain references and do not introduce detailed payloads or modify configuration", () => {
    let state = initialConversation("root");
    const actor: ActorDetail = { id: "root", conversation_url: "/actors/root", status: "idle", created_at_ms: 1, updated_at_ms: 1,
      config: { name: "Original", workspace: "workspace", apps: ["files"], model: null, instructions: "keep", permissions: { approval_mode: "ask", readable_roots: ["workspace"], writable_roots: [] } } };
    state = conversationReducer(state, { type: "actor", actor });
    state = conversationReducer(state, { type: "event", record: event(1, { type: "actor_changed", actor: { id: "root", name: "Updated name", conversation_url: "/actors/root", status: "running", apps: ["files"], created_at_ms: 1, updated_at_ms: 2 } }) });
    state = conversationReducer(state, { type: "event", record: event(2, { type: "tool_started", message_id: "m", call_id: "call", name: "files__read_file" }) });
    state = conversationReducer(state, { type: "event", record: event(3, { type: "tool_finished", message_id: "m", call_id: "call", name: "files__read_file", result_id: "result", summary: "Read file", error: null }) });
    expect(state.actor?.config).toBe(actor.config);
    expect(state.tools).toHaveLength(1);
    expect(state.tools[0].id).toBe("result");
    expect("payload" in state.tools[0]).toBe(false);
  });
  test("send failures reuse the exact idempotency key; editing creates a new operation", () => {
    const first = submissionFor(undefined, "draft", ["a"], () => "key-1");
    expect(submissionFor(first, "draft", ["a"], () => "unexpected")).toBe(first);
    expect(submissionFor(first, "draft changed", ["a"], () => "key-2").key).toBe("key-2");
    expect(first.content).toBe("draft");
    expect(first.attachments).toEqual(["a"]);
  });
  test("late summary pages from a previous Actor cannot enter the selected conversation", () => {
    const state = initialConversation("selected");
    const next = conversationReducer(state, { type: "tools", actorId: "previous", messageId: "m", nextCursor: 1, tools: [{ seq: 1, id: "detail", message_id: "m", call_id: "call", name: "files__read_file", summary: "Private", error: null }] });
    expect(next).toBe(state);
    expect(next.tools).toEqual([]);
    expect(next.toolNext).toEqual({});
  });
  test("tool history pages deduplicate references and retain each message's continuation", () => {
    let state = initialConversation("root");
    const tool = (seq: number) => ({ seq, id: `result-${seq}`, message_id: "m", call_id: `call-${seq}`, name: "files__read_file", summary: "Read", error: null });
    state = conversationReducer(state, { type: "tools", actorId: "root", messageId: "m", nextCursor: 30, tools: [tool(30), tool(31)] });
    expect(state.toolNext.m).toBe(30);
    state = conversationReducer(state, { type: "tools", actorId: "root", messageId: "m", nextCursor: null, tools: [tool(29), tool(30)] });
    expect(state.tools.map(item => item.seq)).toEqual([29, 30, 31]);
    expect(state.tools.every(item => item.status === "completed")).toBe(true);
    expect(state.toolNext.m).toBeNull();
  });
});

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => { resolve = resolvePromise; reject = rejectPromise; });
  return { promise, resolve, reject };
}
const attachment = (name: string): Attachment => ({ id: name, name, media_type: "text/plain", size_bytes: 1, uri: `/resource/${name}` });
const files = (...names: string[]) => names.map(name => new File([name], name));
const flush = async () => { await Promise.resolve(); await Promise.resolve(); };

describe("serialized attachment uploads", () => {
  test("concurrent batches accept more than four files and stay busy until the last batch finishes", async () => {
    const entry: Parameters<typeof queueAttachmentUploads>[0] = { attachments: [] };
    const first = deferred<Attachment>();
    const last = deferred<Attachment>();
    const calls: string[] = [];
    const busy: boolean[] = [];
    const options = {
      signal: new AbortController().signal, isCurrent: () => true,
      upload: (file: File) => { calls.push(file.name); return file.name === "a" ? first.promise : file.name === "d" ? last.promise : Promise.resolve(attachment(file.name)); },
      onChanged: () => busy.push(Boolean(entry.uploadQueue?.pending)),
    };
    const one = queueAttachmentUploads(entry, files("a", "b"), options);
    const two = queueAttachmentUploads(entry, files("c", "d"), options);
    const three = queueAttachmentUploads(entry, files("e", "f"), options);
    await flush();
    expect(calls).toEqual(["a"]);
    expect(entry.attachments).toHaveLength(0);
    first.resolve(attachment("a"));
    await one;
    await flush();
    expect(calls).toEqual(["a", "b", "c", "d"]);
    expect(busy.at(-1)).toBe(true);
    expect(entry.attachments.map(item => item.id)).toEqual(["a", "b", "c"]);
    last.resolve(attachment("d"));
    await two;
    await three;
    expect(entry.attachments.map(item => item.id)).toEqual(["a", "b", "c", "d", "e", "f"]);
    expect(busy.at(-1)).toBe(false);
  });

  test("failure preserves completed uploads without blocking a following batch", async () => {
    const entry = { attachments: [] as Attachment[] };
    const rejected = deferred<Attachment>();
    const options = { signal: new AbortController().signal, isCurrent: () => true,
      upload: (file: File) => file.name === "bad" ? rejected.promise : Promise.resolve(attachment(file.name)), onChanged: () => {} };
    const one = queueAttachmentUploads(entry, files("bad", "unused", "unused-too"), options);
    const failure = one.catch(error => error as Error);
    const two = queueAttachmentUploads(entry, files("good"), options);
    await flush();
    rejected.reject(new Error("failed upload"));
    expect((await failure as Error).message).toBe("failed upload");
    await two;
    await queueAttachmentUploads(entry, files("next", "next-too", "last"), options);
    expect(entry.attachments.map(item => item.id)).toEqual(["good", "next", "next-too", "last"]);
  });

  test("abort and epoch changes discard delayed old results and leave the new Actor queue alone", async () => {
    for (const abortOld of [true, false]) {
      const old = { attachments: [] as Attachment[] };
      const next = { attachments: [] as Attachment[] };
      const oldSignal = new AbortController();
      const delayed = deferred<Attachment>();
      let epoch = 1;
      const stale = queueAttachmentUploads(old, files("old"), { signal: oldSignal.signal, isCurrent: () => epoch === 1, upload: () => delayed.promise, onChanged: () => {} });
      const failure = stale.catch(error => error as Error);
      await flush();
      epoch = 2;
      if (abortOld) oldSignal.abort();
      await queueAttachmentUploads(next, files("new"), { signal: new AbortController().signal, isCurrent: () => epoch === 2, upload: file => Promise.resolve(attachment(file.name)), onChanged: () => {} });
      delayed.resolve(attachment("old"));
      expect((await failure as Error).name).toBe("AbortError");
      expect(old.attachments).toEqual([]);
      expect(next.attachments.map(item => item.id)).toEqual(["new"]);
    }
  });

  test("selected iterables upload every file and array-like impostors are rejected", async () => {
    const entry = { attachments: [] as Attachment[] };
    let uploads = 0;
    const options = { signal: new AbortController().signal, isCurrent: () => true, upload: async (file: File) => { uploads += 1; return attachment(file.name); }, onChanged: () => {} };
    const selected = files("a");
    selected[Symbol.iterator] = function* () { yield* files("a", "b", "c", "d", "e"); };
    await queueAttachmentUploads(entry, selected, options);
    await expect(queueAttachmentUploads(entry, { length: 1, 0: new File(["a"], "a") } as unknown as FileList, options)).rejects.toThrow();
    expect(uploads).toBe(5);
    expect(entry.attachments.map(item => item.id)).toEqual(["a", "b", "c", "d", "e"]);
  });
});
