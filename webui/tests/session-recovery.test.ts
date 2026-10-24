import { afterEach, expect, test } from "bun:test";
import { ActorApiError, restoreSession } from "../src/lib/actor-api";

const originalFetch = globalThis.fetch;
afterEach(() => { globalThis.fetch = originalFetch; });

test("startup renews the existing cookie before restoring the same account session", async () => {
  const calls: { path: string; method: string; marker: string | null; credentials: RequestCredentials | undefined }[] = [];
  globalThis.fetch = (async (path, options) => {
    calls.push({ path: String(path), method: options?.method ?? "GET", marker: new Headers(options?.headers).get("x-daat-request"), credentials: options?.credentials });
    return String(path) === "/v1/account/session" ? new Response(null, {status:204}) : Response.json({kind:"owner",session_id:"persisted-session",manage:true});
  }) as typeof fetch;
  expect(await restoreSession()).toEqual({kind:"owner",session_id:"persisted-session",manage:true});
  expect(calls).toEqual([
    {path:"/v1/account/session",method:"POST",marker:"1",credentials:"same-origin"},
    {path:"/v1/access",method:"GET",marker:"1",credentials:"same-origin"},
  ]);
});

test("temporary startup failures remain retryable and only a confirmed invalid session requests login", async () => {
  for (const response of [new Response(null,{status:503}),new Response(null,{status:403})]) {
    globalThis.fetch = (async () => response) as typeof fetch;
    await expect(restoreSession()).rejects.toBeInstanceOf(ActorApiError);
  }
  globalThis.fetch = (async () => { throw new TypeError("temporarily offline"); }) as typeof fetch;
  await expect(restoreSession()).rejects.toBeInstanceOf(ActorApiError);
  globalThis.fetch = (async () => new Response(null,{status:401})) as typeof fetch;
  expect(await restoreSession()).toBeNull();
  globalThis.fetch = (async path => String(path) === "/v1/account/session" ? new Response(null,{status:204}) : Response.json({kind:"owner",session_id:"restored-after-outage",manage:true})) as typeof fetch;
  expect((await restoreSession())?.session_id).toBe("restored-after-outage");
});
