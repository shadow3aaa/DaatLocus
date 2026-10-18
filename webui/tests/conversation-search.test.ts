import { expect, test } from "bun:test";
import { ConversationSearch, matchingConversationMessages } from "../src/lib/conversation-search";
import type { MessageRecord, Page } from "../src/lib/actor-api";

const message = (seq: number, content: string, reply: string | null = null): MessageRecord => ({seq, id: String(seq), actor_id:"worker", kind:"request", source:{kind:"user"}, reply_to:{kind:"user"}, content, reply, attachments:[], status:"completed", cancel_requested:false, error:null, in_reply_to:null, created_at_ms:seq, updated_at_ms:seq});
function fixture() {
  const requests: {actor: string; query: string; before?: number; signal: AbortSignal; resolve(page: Page<MessageRecord>): void; reject(error: Error): void}[] = [];
  const search = new ConversationSearch("worker", (actor, query, options) => new Promise((resolve, reject) => { requests.push({actor, query, ...options, resolve, reject}); }));
  return {search, requests};
}

test("a late response cannot replace a changed query, even if the transport ignores abort", async () => {
  const {search, requests} = fixture();
  const first = search.search("first");
  const second = search.search("second");
  expect(requests[0].signal.aborted).toBe(true);
  requests[1].resolve({items:[message(9,"second")],next_cursor:null});
  await second;
  requests[0].resolve({items:[message(1,"first")],next_cursor:null});
  await first;
  expect(search.getSnapshot()).toMatchObject({query:"second",items:[{id:"9"}],pending:false,failed:false});
  expect(requests.map(request => request.actor)).toEqual(["worker","worker"]);
});

test("closing search cancels its request and does not publish a late result", async () => {
  const {search, requests} = fixture();
  const pending = search.search("needle");
  search.cancel();
  requests[0].resolve({items:[message(7,"needle")],next_cursor:null});
  await pending;
  expect(requests[0].signal.aborted).toBe(true);
  expect(search.getSnapshot().items).toEqual([]);
});

test("older matches are fetched once per cursor, deduplicated and kept chronological", async () => {
  const {search, requests} = fixture();
  const initial = search.search("needle");
  requests[0].resolve({items:[message(7,"needle"),message(9,"needle")],next_cursor:7});
  await initial;
  const more = search.more(new AbortController().signal);
  await search.more(new AbortController().signal);
  expect(requests).toHaveLength(2);
  expect(requests[1].before).toBe(7);
  requests[1].resolve({items:[message(2,"needle"),message(7,"old duplicate")],next_cursor:null});
  await more;
  expect(search.getSnapshot().items.map(item => [item.seq,item.content])).toEqual([[2,"needle"],[7,"needle"],[9,"needle"]]);
  await search.more(new AbortController().signal);
  expect(requests).toHaveLength(2);
});

test("failed pagination retains its cursor for retry; a failed query can also be retried", async () => {
  const {search, requests} = fixture();
  const failed = search.search("needle");
  requests[0].reject(new Error("unavailable")); await failed;
  expect(search.getSnapshot().failed).toBe(true);
  const retry = search.search("needle");
  requests[1].resolve({items:[message(9,"needle")],next_cursor:9}); await retry;
  const more = search.more(new AbortController().signal);
  requests[2].reject(new Error("unavailable"));
  await expect(more).rejects.toThrow("unavailable");
  expect(search.getSnapshot().next).toBe(9);
  const next = search.more(new AbortController().signal);
  expect(requests[3].before).toBe(9);
  requests[3].resolve({items:[message(3,"needle")],next_cursor:null}); await next;
  expect(search.getSnapshot().items.map(item => item.id)).toEqual(["3","9"]);
});

test("live records update search hits and new matches without reviving nonmatching replies", () => {
  const items = [message(1,"old", "needle"),message(4,"NEEDLE")];
  const live = [message(1,"old", "corrected"),message(6,"new", "needle")];
  expect(matchingConversationMessages(items,live,"needle").map(item => item.seq)).toEqual([4,6]);
});

test("a continuation from an old query cannot insert its records into new results", async () => {
  const {search, requests} = fixture();
  const initial = search.search("first");
  requests[0].resolve({items:[message(9,"first")],next_cursor:9}); await initial;
  const older = search.more(new AbortController().signal);
  const changed = search.search("second");
  expect(requests[1].signal.aborted).toBe(true);
  requests[2].resolve({items:[message(7,"second")],next_cursor:null}); await changed;
  requests[1].resolve({items:[message(1,"first")],next_cursor:null}); await older;
  expect(search.getSnapshot().items.map(item => item.content)).toEqual(["second"]);
});
