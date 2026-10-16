import { convertFileSrc, invoke, isTauri } from "@tauri-apps/api/core";

type Reply = { status: number; headers: [string, string][] };
const active = new Set<number>();
const cancel = (id: number) => {
  active.delete(id);
  return invoke<void>("native_http_cancel", { id }).catch(() => {});
};
if (typeof window !== "undefined") window.addEventListener("pagehide", () => {
  for (const id of active) void cancel(id);
});

/** One HTTP contract; the native adapter keeps cookies outside JavaScript. */
export const serviceFetch: typeof fetch = async (input, init) => {
  if (!isTauri()) return fetch(input, init);
  const request = new Request(input instanceof Request ? input : new URL(String(input), location.href), init);
  const url = new URL(request.url), local = new URL(location.href);
  if (url.protocol !== local.protocol || url.host !== local.host || !url.pathname.startsWith("/v1/"))
    throw new TypeError("Service URL is outside this client");
  request.signal.throwIfAborted();
  const headers = Array.from(request.headers).filter(([name]) => name !== "x-daat-request");
  const id = await invoke<number>("native_http_start", new Uint8Array(), {
    headers: { "x-daat-http": JSON.stringify({ path: url.pathname + url.search, method: request.method, headers }) },
  });
  active.add(id);
  const upload = request.body?.getReader();
  const abort = () => { void cancel(id); void upload?.cancel().catch(() => {}); };
  request.signal.addEventListener("abort", abort, { once: true });
  const release = async () => {
    request.signal.removeEventListener("abort", abort);
    await cancel(id);
  };
  try {
    request.signal.throwIfAborted();
    if (upload) {
      try {
        for (;;) {
          const chunk = await upload.read();
          if (chunk.done) break;
          for (let offset = 0; offset < chunk.value.length; offset += 256 * 1024) {
            request.signal.throwIfAborted();
            await invoke("native_http_write", chunk.value.subarray(offset, offset + 256 * 1024), {
              headers: { "x-daat-http-id": String(id) },
            });
          }
        }
      } finally { upload.releaseLock(); }
    }
    request.signal.throwIfAborted();
    const reply = await invoke<Reply>("native_http_send", { id });
    request.signal.throwIfAborted();
    if (request.method === "HEAD" || [204, 205, 304].includes(reply.status)) {
      await release();
      return new Response(null, reply);
    }
    const body = new ReadableStream<Uint8Array>({
      async pull(controller) {
        try {
          const chunk = await invoke<ArrayBuffer | number[]>("native_http_read", { id });
          request.signal.throwIfAborted();
          const bytes = chunk instanceof ArrayBuffer ? new Uint8Array(chunk) : Uint8Array.from(chunk);
          if (bytes.length) controller.enqueue(bytes);
          else { controller.close(); await release(); }
        } catch (error) { controller.error(error); await release(); }
      },
      async cancel() { await release(); },
    });
    return new Response(body, reply);
  } catch (error) {
    await release();
    throw error;
  }
};

/** Only authorized service resources may use the credentialed native protocol. */
export function resourceSource(path: string): string {
  if (!isTauri()) return path;
  const url = new URL(path, location.href), local = new URL(location.href);
  if (url.protocol !== local.protocol || url.host !== local.host || url.search || url.hash
    || !/^\/v1\/(?:stages\/[0-9a-f-]+\/)?actors\/[a-zA-Z0-9_-]+\/resources\/[^/%\\]+$/.test(url.pathname))
    return "";
  return new URL(url.pathname, convertFileSrc("", "dl-resource")).href;
}

export async function downloadResource(path: string, name: string): Promise<boolean> {
  if (!isTauri() || !resourceSource(path)) throw new TypeError("Invalid native resource");
  return invoke<boolean>("native_resource_download", { path: new URL(path, location.href).pathname, name });
}
