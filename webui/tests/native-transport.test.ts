import { afterEach, beforeEach, expect, test } from "bun:test";
import { resourceSource, serviceFetch } from "../src/lib/transport";
import { actorIdFromUrl, resourceActorId } from "../src/lib/conversation-links";

const keys = ["window", "location", "isTauri"] as const;
let saved: (PropertyDescriptor | undefined)[];
let handler: (command: string, args: unknown, options?: { headers?: Record<string, string> }) => unknown;
beforeEach(() => {
  saved = keys.map(key => Object.getOwnPropertyDescriptor(globalThis, key));
  Object.defineProperty(globalThis, "isTauri", { configurable: true, value: true });
  Object.defineProperty(globalThis, "location", { configurable: true, value: new URL("tauri://localhost/index.html") });
  Object.defineProperty(globalThis, "window", { configurable: true, value: {
    __TAURI_INTERNALS__: {
      invoke: (...args: Parameters<typeof handler>) => Promise.resolve().then(() => handler(...args)),
      convertFileSrc: (path: string, protocol: string) => `${protocol}://localhost/${encodeURIComponent(path)}`,
    },
  } });
});
afterEach(() => keys.forEach((key, index) => {
  const prior = saved[index];
  if (prior) Object.defineProperty(globalThis, key, prior);
  else Reflect.deleteProperty(globalThis, key);
}));

test("native requests preserve binary input and reconstruct incrementally read UTF-8 output", async () => {
  const chunks = [new Uint8Array([228, 189]).buffer, new Uint8Array([160, 0, 240, 159, 153, 130]).buffer, new ArrayBuffer(0)];
  const raw = new Uint8Array([0, 255, 13, 10]);
  let reads = 0;
  const cancelled: number[] = [];
  handler = (command, args, options) => {
    if (command === "native_http_start") {
      expect((args as Uint8Array).byteLength).toBe(0);
      expect(JSON.parse(options!.headers!["x-daat-http"])).toMatchObject({ path: "/v1/actors/core/attachments", method: "POST" });
      return 7;
    }
    if (command === "native_http_write") {
      expect(new Uint8Array(args as Uint8Array)).toEqual(raw);
      expect(options!.headers!["x-daat-http-id"]).toBe("7");
    }
    if (command === "native_http_send") return { status: 200, headers: [["content-type", "application/octet-stream"]] };
    if (command === "native_http_read") { reads++; return chunks.shift(); }
    if (command === "native_http_cancel") cancelled.push((args as { id: number }).id);
  };
  const response = await serviceFetch("/v1/actors/core/attachments", { method: "POST", body: raw });
  await Promise.resolve();
  expect(reads).toBeLessThanOrEqual(1);
  expect(await response.text()).toBe("你\0🙂");
  expect(cancelled).toContain(7);
});

test("large native uploads use bounded chunks and abort before sending the HTTP action", async () => {
  const raw = Uint8Array.from({ length: 700_000 }, (_, index) => index % 251);
  const uploaded: Uint8Array[] = [];
  handler = (command, args) => {
    if (command === "native_http_start") return 8;
    if (command === "native_http_write") {
      const chunk = args as Uint8Array;
      expect(chunk.byteLength).toBeLessThanOrEqual(256 * 1024);
      uploaded.push(chunk.slice());
    }
    if (command === "native_http_send") {
      const restored = new Uint8Array(uploaded.reduce((total, chunk) => total + chunk.length, 0));
      let offset = 0;
      for (const chunk of uploaded) { restored.set(chunk, offset); offset += chunk.length; }
      expect(restored).toEqual(raw);
      return { status: 204, headers: [] };
    }
  };
  expect((await serviceFetch("/v1/actors/core/attachments", { method: "POST", body: raw })).status).toBe(204);
  expect(uploaded.length).toBeGreaterThan(1);

  const controller = new AbortController();
  let sent = false, cancelled = false;
  handler = command => {
    if (command === "native_http_start") return 10;
    if (command === "native_http_write") controller.abort();
    if (command === "native_http_send") sent = true;
    if (command === "native_http_cancel") cancelled = true;
  };
  await expect(serviceFetch("/v1/actors/core/attachments", { method: "POST", body: raw, signal: controller.signal })).rejects.toThrow();
  expect(sent).toBe(false);
  expect(cancelled).toBe(true);
});

test("aborting a pending native body read releases the native request", async () => {
  const controller = new AbortController();
  let rejectRead: (reason: Error) => void = () => {};
  let reading!: () => void;
  const started = new Promise<void>(resolve => { reading = resolve; });
  const cancelled: number[] = [];
  handler = (command, args) => {
    if (command === "native_http_start") return 9;
    if (command === "native_http_send") return { status: 200, headers: [] };
    if (command === "native_http_read") return new Promise((_resolve, reject) => { rejectRead = reject; reading(); });
    if (command === "native_http_cancel") { cancelled.push((args as { id: number }).id); rejectRead(new Error("cancelled")); }
  };
  const response = await serviceFetch("/v1/event-stream", { signal: controller.signal });
  const text = response.text();
  await started;
  controller.abort();
  await expect(text).rejects.toThrow();
  expect(cancelled).toContain(9);
});

test("HEAD and no-content replies never consume a native response body", async () => {
  for (const [method, status] of [["HEAD", 200], ["POST", 204]] as const) {
    handler = command => {
      if (command === "native_http_start") return 11;
      if (command === "native_http_send") return { status, headers: [["content-length", "234"]] };
      if (command === "native_http_read") throw new Error("Unexpected body read");
    };
    const response = await serviceFetch("/v1/account/session", { method });
    expect(response.status).toBe(status);
    expect(response.body).toBeNull();
    expect(response.headers.get("content-length")).toBe("234");
  }
});

test("native paths cannot become arbitrary network requests or resource reads", async () => {
  handler = () => { throw new Error("No IPC expected"); };
  for (const path of ["https://example.com/v1/access", "file:///v1/access", "/private"]) {
    await expect(serviceFetch(path)).rejects.toThrow();
  }
  expect(resourceSource("/v1/actors/core/resources/image")).toBe("dl-resource://localhost/v1/actors/core/resources/image");
  for (const path of ["/v1/account", "https://example.com/v1/actors/core/resources/image", "/v1/actors/core/resources/image?token=x"])
    expect(resourceSource(path)).toBe("");
  expect(actorIdFromUrl("/actors/work", "tauri://localhost/index.html")).toBe("work");
  expect(resourceActorId("/v1/actors/core/resources/image", "tauri://localhost/index.html")).toBe("core");
  expect(actorIdFromUrl("other://localhost/actors/work", "tauri://localhost")).toBeNull();
});


test("browser page detaches while navigation is pending and keeps layout changes ordered", async () => {
  const {browserControl,browserLayout}=await import("../src/lib/browser-workspace");
  const actions:string[]=[];
  let started!:()=>void;const opening=new Promise<void>(resolve=>{started=resolve;});
  let finish!:()=>void;const navigating=new Promise<void>(resolve=>{finish=resolve;});
  handler=async(command,args)=>{
    expect(command).toBe("browser_control");
    const action=(args as {action:string}).action;actions.push(action);
    if(action==="open"){started();await navigating;}
    return {pages:[],selected:null};
  };
  const open=browserControl("open",null,"https://example.com");
  await opening;
  await Promise.all([browserLayout({x:80,y:120,width:800,height:600,scale:1}),browserLayout(null)]);
  expect(actions).toEqual(["open","mount","detach"]);
  finish();await open;
});
