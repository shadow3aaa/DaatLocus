import { expect, test } from "bun:test";
import { DelegationStatuses, delegationsOf } from "../src/lib/delegations";
import { configureConversationDevice } from "../src/lib/conversation-links";
import type { EventRecord, MessageStatus, ToolSummary } from "../src/lib/actor-api";

const device = "11111111-2222-4333-8444-555555555555";
const remote = "99999999-8888-4777-8666-555555555555";
const tool = (id: string, patch: Partial<ToolSummary> = {}, outcome: Record<string, unknown> = {}): ToolSummary => ({
  seq: 1, id, message_id: "core-input", call_id: id, name: "actors__message", summary: "", error: null,
  outcome: { sent_message: true, message_id: `request-${id}`, actor_id: "atlas", conversation_url: "/actors/atlas", ...outcome } as ToolSummary["outcome"], ...patch,
});

test("only delivered requests become delegations, addressed by the recipient's device", () => {
  configureConversationDevice(device);
  try {
    const items = delegationsOf([
      tool("local", {}, { title: "Fix login", name: "Atlas" }),
      tool("qualified-local", {}, { conversation_url: `/stages/${device}/actors/iris`, actor_id: "iris" }),
      tool("remote", { name: "actors__create" }, { conversation_url: `/stages/${remote}/actors/hermes`, actor_id: "hermes", reply_to: "earlier" }),
      tool("failed", { error: "Actor archived" }),
      tool("not-sent", {}, { sent_message: undefined }),
      tool("status", { name: "actors__status" }),
      tool("created-only", { name: "actors__create" }, { sent_message: undefined, message_id: undefined }),
    ]);
    expect(items.map(item => [item.resultId, item.actorId, item.messageId, item.followUp])).toEqual([
      ["local", "atlas", "request-local", false],
      ["qualified-local", "iris", "request-qualified-local", false],
      ["remote", `${remote}~hermes`, "request-remote", true],
    ]);
    expect(items[0]).toMatchObject({ title: "Fix login", name: "Atlas" });
  } finally { configureConversationDevice(null); }
});

test("status facts never regress and only tracked local requests follow the catalog stream", async () => {
  const reads: [string, string][] = [];
  const answers = new Map<string, (status: MessageStatus) => void>();
  const store = new DelegationStatuses((actor, message) => { reads.push([actor, message]); return new Promise(resolve => answers.set(message, resolve)); }, 2);
  const updated = (actor_id: string, message_id: string, status: MessageStatus): EventRecord => ({ seq: 1, actor_id, created_at_ms: 1, event: { type: "message_updated", message_id, status } });
  const untrack = store.track("atlas", "one");
  store.track("atlas", "two");
  store.track("atlas", "three");
  expect(reads).toEqual([["atlas", "one"], ["atlas", "two"]]);
  store.observe(updated("atlas", "one", "completed"));
  expect(store.status("atlas", "one")).toBe("completed");
  // A slower read started before completion cannot move the request back.
  answers.get("one")!("running");
  await new Promise(resolve => setTimeout(resolve, 0));
  expect(store.status("atlas", "one")).toBe("completed");
  expect(reads.map(([, message]) => message)).toEqual(["one", "two", "three"]);
  store.observe(updated("other", "two", "failed"));
  expect(store.status("atlas", "two")).toBeUndefined();
  untrack();
  store.observe(updated("atlas", "four", "running"));
  expect(store.status("atlas", "four")).toBeUndefined();
  answers.get("two")!("running");
  answers.get("three")!("queued");
  await new Promise(resolve => setTimeout(resolve, 0));
  reads.length = 0;
  store.refreshUnfinished();
  expect(reads).toEqual([["atlas", "two"], ["atlas", "three"]]);
});
