import type { EventRecord } from "./actor-api";
import { serviceFetch } from "./transport";

export type StreamStatus = "connecting" | "connected" | "reconnecting" | "closed";
export type StreamOptions = {
  after?: number;
  signal?: AbortSignal;
  retryMs?: number;
  fetcher?: typeof fetch;
  onEvent(record: EventRecord): void | Promise<void>;
  onCursor?(cursor: number): void | Promise<void>;
  onStatus?(status: StreamStatus): void;
  onAccessRevoked?(): void;
  onResync?(): void;
};
type Frame = {event: string; id?: string; data: string; retry?: number};
const MAX_FRAME_BYTES = 2 * 1024 * 1024;
const encoder = new TextEncoder();

/** Incremental SSE decoding; CRLF and data lines can span arbitrary transport chunks. */
export class SseParser {
  private buffer = "";
  private event = "message";
  private id: string | undefined;
  private retry: number | undefined;
  private data: string[] = [];
  private size = 0;

  push(chunk: string): Frame[] {
    this.buffer += chunk;
    if (this.buffer.length + this.size > MAX_FRAME_BYTES || encoder.encode(this.buffer).byteLength + this.size > MAX_FRAME_BYTES) throw new Error("SSE frame exceeds byte limit");
    const frames: Frame[] = [];
    for (;;) {
      const newline = this.buffer.search(/[\r\n]/);
      if (newline < 0 || this.buffer[newline] === "\r" && newline === this.buffer.length - 1) break;
      const line = this.buffer.slice(0, newline);
      const skip = this.buffer[newline] === "\r" && this.buffer[newline + 1] === "\n" ? 2 : 1;
      this.buffer = this.buffer.slice(newline + skip);
      this.size += encoder.encode(line).byteLength;
      if (!line) {
        if (this.data.length) frames.push({event:this.event, id:this.id, data:this.data.join("\n"), retry:this.retry});
        this.event = "message"; this.id = undefined; this.retry = undefined; this.data = []; this.size = 0;
      } else if (!line.startsWith(":")) {
        const colon = line.indexOf(":"), field = colon < 0 ? line : line.slice(0,colon);
        const value = colon < 0 ? "" : line.slice(colon+1).replace(/^ /, "");
        if (field === "event") this.event = value;
        else if (field === "data") this.data.push(value);
        else if (field === "id" && !value.includes("\0")) this.id = value;
        else if (field === "retry" && /^\d+$/.test(value)) this.retry = Number(value);
      }
    }
    return frames;
  }
}

function pause(ms: number, signal?: AbortSignal): Promise<void> {
  return new Promise(resolve => {
    if (signal?.aborted) return resolve();
    const done = () => { clearTimeout(timer); signal?.removeEventListener("abort", done); resolve(); };
    const timer = setTimeout(done, ms);
    signal?.addEventListener("abort", done, {once:true});
  });
}

/** Commit cursors only after consumers accept source events; wait for consumer backpressure. */
export async function subscribe(path: string, options: StreamOptions): Promise<void> {
  let cursor = options.after ?? 0;
  let retry = options.retryMs ?? 1000;
  const fetcher = options.fetcher ?? serviceFetch;
  while (!options.signal?.aborted) {
    options.onStatus?.(cursor ? "reconnecting" : "connecting");
    let reader: ReadableStreamDefaultReader<Uint8Array> | undefined;
    const cancelRead = () => { void reader?.cancel().catch(() => {}); };
    try {
      const response = await fetcher(path, {
        credentials:"same-origin", cache:"no-store", signal:options.signal,
        headers:{Accept:"text/event-stream", "Last-Event-ID":String(cursor)},
      });
      if ([401,403,410].includes(response.status)) { options.onAccessRevoked?.(); return; }
      if ([409,422].includes(response.status)) { options.onResync?.(); return; }
      if (!response.ok || !response.body) throw new Error("Event stream unavailable");
      reader = response.body.getReader();
      options.signal?.addEventListener("abort", cancelRead, {once:true});
      if (options.signal?.aborted) { cancelRead(); break; }
      options.onStatus?.("connected");
      const parser = new SseParser(), decoder = new TextDecoder();
      while (!options.signal?.aborted) {
        const chunk = await reader.read();
        if (chunk.done) break;
        for (const frame of parser.push(decoder.decode(chunk.value,{stream:true}))) {
          if (options.signal?.aborted) break;
          if (frame.event === "resync") { options.onResync?.(); return; }
          if (frame.event === "access_revoked") { options.onAccessRevoked?.(); return; }
          if (frame.retry != null) retry = Math.min(30_000,Math.max(100,frame.retry));
          const id = Number(frame.id);
          if (frame.id == null || !Number.isSafeInteger(id) || id <= cursor) continue;
          if (frame.event === "activity") {
            const record = JSON.parse(frame.data) as EventRecord;
            if (record.seq !== id) throw new Error("Event cursor mismatch");
            await options.onEvent(record);
          } else if (frame.event !== "cursor") continue;
          await options.onCursor?.(id);
          cursor = id;
        }
      }
    } catch (error) {
      if (options.signal?.aborted) break;
      options.onStatus?.("reconnecting");
    } finally {
      options.signal?.removeEventListener("abort", cancelRead);
      await reader?.cancel().catch(() => {});
      reader?.releaseLock();
    }
    if (!options.signal?.aborted) await pause(retry,options.signal);
  }
  options.onStatus?.("closed");
}
