import { afterEach, expect, test } from "bun:test";
import { listRunningSessionActors } from "../src/lib/session-api";

const originalFetch = globalThis.fetch;
afterEach(() => { globalThis.fetch = originalFetch; });

test("running workers page through the remote host without dropping the first event cursor", async () => {
  const host = "00000000-0000-4000-8000-000000000001";
  const requests: string[] = [];
  globalThis.fetch = (async (input: string | URL | Request) => {
    const url = new URL(String(input), "http://localhost");
    requests.push(url.pathname + url.search);
    expect(url.searchParams.get("status")).toBe("running");
    expect(url.searchParams.get("limit")).toBe("50");
    return Response.json(url.searchParams.has("after")
      ? {items: [{id: "worker-b", name: "Mobile check"}], next_cursor: null, event_cursor: 49}
      : {items: [{id: "worker-a", name: "Layout check"}], next_cursor: "worker-a", event_cursor: 41});
  }) as typeof fetch;
  const page = await listRunningSessionActors(`${host}~session`);
  expect(requests).toHaveLength(2);
  expect(requests.every(path => new URL(path, "http://localhost").pathname === `/v1/stages/${host}/sessions/session/actors`)).toBe(true);
  expect(new URL(requests[1], "http://localhost").searchParams.get("after")).toBe("worker-a");
  expect(page.items.map(actor => actor.id)).toEqual(["worker-a", "worker-b"]);
  expect(page.event_cursor).toBe(41);
  expect(page.next_cursor).toBeNull();
});

test("a failed later worker page does not publish an incomplete running count", async () => {
  globalThis.fetch = (async (input: string | URL | Request) => {
    const url = new URL(String(input), "http://localhost");
    return url.searchParams.has("after")
      ? Response.json({code: "unavailable"}, {status: 503})
      : Response.json({items: [{id: "worker-a"}], next_cursor: "worker-a", event_cursor: 41});
  }) as typeof fetch;
  await expect(listRunningSessionActors("session")).rejects.toMatchObject({status: 503});
});
