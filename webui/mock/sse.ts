/**
 * Server side of the SSE protocol enforced by `src/lib/event-stream.ts`:
 * - client reconnects with `Last-Event-ID: <n>`; we replay stored events with
 *   seq > n, then push live events;
 * - activity frames carry `id: <seq>` equal to `data.seq`;
 * - idle connections get a `: keepalive` comment every 5s (the real server
 *   never sends `retry:` fields; cursor frames only appear when a batch made
 *   progress without visible events — the mock has no hidden events);
 * - a `resync` frame (after a mock reset) tells the client to reload.
 */
import type { EventRecord } from "../src/lib/actor-api";
import { addListener, maxSeq, streamEvents, type StreamItem } from "./state";

const encoder = new TextEncoder();
const KEEPALIVE_MS = 5_000;

function activityFrame(record: EventRecord): string {
  return `id: ${record.seq}\nevent: activity\ndata: ${JSON.stringify(record)}\n\n`;
}

export function sseResponse(streamKey: string, lastEventIdHeader: string | null): Response {
  const parsed = Number.parseInt(lastEventIdHeader ?? "", 10);
  // Backend (src/service/stream.rs): a cursor below 0 or ahead of the current
  // position is rejected with 422 invalid_request; anything else replays.
  if (Number.isSafeInteger(parsed) && (parsed < 0 || parsed > maxSeq(streamKey)))
    return new Response(JSON.stringify({ message: "Request is invalid", code: "invalid_request" }), {
      status: 422,
      headers: { "Content-Type": "application/json" },
    });
  const after = Number.isSafeInteger(parsed) && parsed > 0 ? parsed : 0;

  let remove: (() => void) | null = null;
  let keepalive: ReturnType<typeof setInterval> | null = null;

  const body = new ReadableStream<Uint8Array>({
    start(controller) {
      let closed = false;
      let lastSent = after;
      const send = (text: string): void => {
        if (closed) return;
        try {
          controller.enqueue(encoder.encode(text));
        } catch {
          closed = true;
        }
      };
      // Replay stored events first, then announce the cursor: a cursor frame
      // with a future seq would make the client drop the replayed events.
      for (const record of streamEvents(streamKey, after)) {
        send(activityFrame(record));
        lastSent = record.seq;
      }
      // Initial cursor frame lets the client commit its position even when the
      // replay was empty. No `retry:` field — the client default applies.
      send(`id: ${lastSent}\nevent: cursor\ndata: {}\n\n`);
      remove = addListener(streamKey, (item: StreamItem) => {
        if (closed) return;
        if ("resync" in item) {
          send("event: resync\ndata: {}\n\n");
          return;
        }
        if (item.seq <= lastSent) return;
        send(activityFrame(item));
        lastSent = item.seq;
      });
      keepalive = setInterval(() => {
        send(": keepalive\n\n");
      }, KEEPALIVE_MS);
    },
    cancel() {
      remove?.();
      remove = null;
      if (keepalive !== null) clearInterval(keepalive);
      keepalive = null;
    },
  });

  return new Response(body, {
    status: 200,
    headers: {
      "Content-Type": "text/event-stream; charset=utf-8",
      "Cache-Control": "no-store",
      Connection: "keep-alive",
      "X-Accel-Buffering": "no",
    },
  });
}
