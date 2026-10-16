import { SseParser } from "../src/lib/event-stream";
import { afterEach, describe, expect, test } from "bun:test";
import { ActorApiError, actorModelReady, listToolSummaries, logoutSession, patchActorPreferences, saveConfig, subscribeActorEvents, uploadAttachment, type ActorDetail, type EventRecord } from "../src/lib/actor-api";
const originalFetch = globalThis.fetch;
test("remote conversations use host readiness even when this phone has no usable model", () => {
  const actor: ActorDetail = {id:"host~task",remote:true,model_ready:true,conversation_url:"/stages/host/actors/task",status:"idle",created_at_ms:1,updated_at_ms:1};
  expect(actorModelReady(actor, false)).toBe(true);
  expect(actorModelReady({...actor, model_ready:false}, true)).toBe(false);
  expect(actorModelReady({...actor, remote:false}, false)).toBe(false);
  expect(actorModelReady({...actor, model_ready:undefined}, false)).toBe(true);
});
afterEach(() => { globalThis.fetch = originalFetch; });
test("renaming a provider carries its credential reference without exposing a stored key", async () => {
  let sent: Record<string, unknown> = {};
  globalThis.fetch = (async (_url: string | URL | Request, init?: RequestInit) => { sent = JSON.parse(String(init?.body)); return Response.json({kind:"complete"}); }) as typeof fetch;
  await saveConfig({providers:[{kind:"openai",name:"new-name",credential_name:"original-name",credential_configured:true,api_key:null}],models:[{name:"model",provider_name:"new-name",model_id:"model"}],main_model:"model"});
  expect(sent.providers).toEqual([{kind:"openai",name:"new-name",credential_name:"original-name",api_key:null}]);
});
const encoder = new TextEncoder();
const record = (seq: number): EventRecord => ({ seq, actor_id: "root", created_at_ms: 1,
  event: { type: "generation_delta", message_id: "message", generation: 1, channel: "assistant", text: "你好" } });
const frame = (seq: number) => `id: ${seq}\nevent: activity\ndata: ${JSON.stringify(record(seq))}\n\n`;
const response = (text: string) => new Response(new ReadableStream<Uint8Array>({ start(controller) {
    const bytes = encoder.encode(text);
    for (let i = 0; i < bytes.length; i += 3)
      controller.enqueue(bytes.slice(i, i + 3));
    controller.close();
  } }), { headers: { "Content-Type": "text/event-stream" } });
describe("incremental SSE", () => {
  test("partial CRLF and multiline data preserve the full source", () => {
    const parser = new SseParser();
    expect(parser.push(": keepalive\r\n\r\nid: 12\r")).toEqual([]);
    expect(parser.push("\nevent: activity\r\ndata: first\r\ndata: second\r\n\r")).toEqual([]);
    expect(parser.push("\n")).toEqual([{ event: "activity", id: "12", data: "first\nsecond", retry: undefined }]);
  });
  test("cursor frames have no artificial business payload and huge partial frames fail", () => {
    const parser = new SseParser();
    expect(parser.push("event: cursor\nid: 9\ndata: {}\n\n")[0].id).toBe("9");
    expect(() => parser.push("x".repeat(2 * 1024 * 1024 + 1))).toThrow();
    expect(() => new SseParser().push("字".repeat(800_000))).toThrow();
  });
  test("remote resync restarts bounded queries instead of retrying a stale stream", async () => {
    let resync = 0;
    let calls = 0;
    const fetcher = (async () => { calls++; return response("event: resync\ndata: {}\n\n"); }) as typeof fetch;
    await subscribeActorEvents("remote", {fetcher,onEvent:()=>{},onResync:()=>{resync++;}});
    expect(resync).toBe(1);expect(calls).toBe(1);
  });
  test("disconnect resumes only consumed IDs and duplicate replay is ignored", async () => {
    const abort = new AbortController();
    const seen: number[] = [];
    const requests: RequestInit[] = [];
    let calls = 0;
    const fetcher = (async (_url: string | URL | Request, init?: RequestInit) => {
      requests.push(init!);
      calls += 1;
      return response(calls === 1 ? frame(1) : frame(1) + frame(2));
    }) as typeof fetch;
    await subscribeActorEvents("root", { signal: abort.signal, retryMs: 1, fetcher,
      onEvent: event => { seen.push(event.seq); if (event.seq === 2)
        abort.abort(); } });
    expect(seen).toEqual([1, 2]);
    expect(new Headers(requests[1].headers).get("Last-Event-ID")).toBe("1");
    expect(requests.every(request => request.credentials === "same-origin")).toBe(true);
  });
  test("handler failure retries the unconsumed event instead of advancing the cursor", async () => {
    const abort = new AbortController();
    let attempts = 0;
    const ids: string[] = [];
    const fetcher = (async (_url: string | URL | Request, init?: RequestInit) => {
      ids.push(new Headers(init?.headers).get("Last-Event-ID")!);
      return response(frame(4));
    }) as typeof fetch;
    await subscribeActorEvents("root", { after: 3, signal: abort.signal, retryMs: 1, fetcher,
      onEvent: () => { attempts += 1; if (attempts === 1)
        throw new Error("detail fetch failed"); abort.abort(); } });
    expect(ids).toEqual(["3", "3"]);
  });
  test("abort cancels a pending body read, without opening another stream", async () => {
    const abort = new AbortController();
    let cancelled = false;
    let calls = 0;
    const fetcher = (async () => { calls += 1; return new Response(new ReadableStream<Uint8Array>({ cancel() { cancelled = true; } })); }) as typeof fetch;
    const running = subscribeActorEvents("root", { signal: abort.signal, fetcher, onEvent: () => { },
      onStatus: status => { if (status === "connected")
        queueMicrotask(() => abort.abort()); } });
    await running;
    expect(cancelled).toBe(true);
    expect(calls).toBe(1);
  });
  test("revoked access stops rather than entering reconnect", async () => {
    let revoked = 0;
    let calls = 0;
    const fetcher = (async () => { calls += 1; return response("event: access_revoked\ndata: {}\n\n"); }) as typeof fetch;
    await subscribeActorEvents("root", { fetcher, onEvent: () => { }, onAccessRevoked: () => { revoked += 1; } });
    expect(revoked).toBe(1);
    expect(calls).toBe(1);
  });
});
describe("cookie-only requests", () => {
  test("accepted cancellation has no response body and still reports denied cancellation", async () => {
    globalThis.fetch = (async () => new Response(null, { status: 202 })) as typeof fetch;
    const { cancelMessage } = await import("../src/lib/actor-api");
    expect(await cancelMessage("root", "message")).toBeUndefined();
    globalThis.fetch = (async () => Response.json({ code: "forbidden", message: "Cancellation is forbidden" }, { status: 403 })) as typeof fetch;
    await expect(cancelMessage("root", "message")).rejects.toBeInstanceOf(ActorApiError);
  });
  test("account logout use guarded HttpOnly sessions", async () => {
    const requests: {
      url: string;
      init: RequestInit;
    }[] = [];
    globalThis.fetch = (async (url: string | URL | Request, init?: RequestInit) => {
      requests.push({ url: String(url), init: init! });
      return new Response(null, { status: 204 });
    }) as typeof fetch;
    await logoutSession();
    expect(requests[0].url).toBe("/v1/account/session");
    expect(requests[0].init.method).toBe("DELETE");
    for (const { init } of requests) {
      expect(init.credentials).toBe("same-origin");
      expect(new Headers(init.headers).get("X-Daat-Request")).toBe("1");
      expect(new Headers(init.headers).has("Authorization")).toBe(false);
    }
  });
  test("Actor preferences PATCH sends only requested fields and tool pages retain bounded continuation", async () => {
    const requests: { url: string; init: RequestInit }[] = [];
    globalThis.fetch = (async (url: string | URL | Request, init?: RequestInit) => {
      requests.push({ url: String(url), init: init! });
      return Response.json({});
    }) as typeof fetch;
    await patchActorPreferences("actor/path", { approval_mode: "none" });
    await patchActorPreferences("actor/path", { model: null });
    await listToolSummaries("root", "message", { before: 24, limit: 30 });
    expect(requests[0].url).toBe("/v1/actors/actor%2Fpath");
    expect(requests[0].init.method).toBe("PATCH");
    expect(JSON.parse(String(requests[0].init.body))).toEqual({ approval_mode: "none" });
    expect(JSON.parse(String(requests[1].init.body))).toEqual({ model: null });
    expect(requests[2].url).toBe("/v1/actors/root/messages/message/results?before=24&limit=30");
  });
  test("error response rejects with a typed code and parsed retry delay", async () => {
    globalThis.fetch = (async () => Response.json({ code: "rate_limited", message: "Too many requests" }, { status: 429, headers: { "Retry-After": "3" } })) as typeof fetch;
    await expect(logoutSession()).rejects.toMatchObject({
      code: "rate_limited", status: 429, retryAfterMs: 3000,
    });
  });
  test("Unicode attachment names have an ASCII transport representation", async () => {
    let headers = new Headers();
    globalThis.fetch = (async (_url: string | URL | Request, init?: RequestInit) => { headers = new Headers(init?.headers); return Response.json({ id: "upload" }); }) as typeof fetch;
    await uploadAttachment("root", new File(["内容"], "报告.txt", { type: "text/plain" }));
    expect(headers.get("X-File-Name")).toBe("%E6%8A%A5%E5%91%8A.txt");
    expect(headers.get("X-File-Name-Encoding")).toBe("utf-8-percent");
  });
});
