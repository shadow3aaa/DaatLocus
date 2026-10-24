import { once } from "node:events";
import path from "node:path";
import { Readable } from "node:stream";
import type { ServerResponse } from "node:http";
import type { Plugin } from "vite";
import { handle } from "./routes";
import { activateScenario } from "./scenarios";

/** One-process development server; no Rust daemon, WASM compiler, or API proxy. */
export function mockPreviewPlugin(): Plugin {
  const responses = new Set<ServerResponse>();
  return {
    name: "daat-locus-mock-preview",
    apply: "serve",
    enforce: "pre",
    closeBundle() {
      for (const response of responses) response.destroy();
      responses.clear();
    },
    resolveId(id) {
      if (process.env.DAAT_LOCUS_MOCK_MACOS === "1" && (id === "@/lib/desktop-window" || id.replaceAll("\\", "/").endsWith("/src/lib/desktop-window"))) return path.resolve(import.meta.dirname, "desktop-window.ts");
      if (process.env.DAAT_LOCUS_MOCK_MACOS === "1" && (id === "@/lib/macos-computer-use" || id.replaceAll("\\", "/").endsWith("/src/lib/macos-computer-use"))) return path.resolve(import.meta.dirname, "macos-computer-use.ts");
      if (process.env.DAAT_LOCUS_MOCK_PHONE === "1") {
        const normalized = id.replaceAll("\\", "/");
        if (id === "@/lib/phone-use" || normalized.endsWith("/src/lib/phone-use")) return path.resolve(import.meta.dirname, "phone-use.ts");
        if (id === "@/lib/mobile" || normalized.endsWith("/src/lib/mobile")) return path.resolve(import.meta.dirname, "mobile.ts");
      }
      if (id === "@/generated/account-pake/daat_locus_auth" ||
          id.replaceAll("\\", "/").endsWith("/src/generated/account-pake/daat_locus_auth")) {
        return path.resolve(import.meta.dirname, "account-pake.ts");
      }
    },
    configureServer(server) {
      activateScenario("conversation");
      server.middlewares.use(async (incoming, outgoing, next) => {
        const pathname = (incoming.url ?? "/").split("?")[0];
        if (!pathname.startsWith("/v1/") && !pathname.startsWith("/__mock")) return next();
        responses.add(outgoing);
        outgoing.once("close", () => responses.delete(outgoing));
        try {
          const headers = new Headers();
          for (const [name, value] of Object.entries(incoming.headers)) {
            if (value != null) headers.set(name, Array.isArray(value) ? value.join(", ") : value);
          }
          const body = !["GET", "HEAD"].includes(incoming.method ?? "GET")
            ? Readable.toWeb(incoming) as ReadableStream<Uint8Array> : undefined;
          const request = new Request(`http://127.0.0.1${incoming.url}`, {
            method: incoming.method, headers,
            ...(body ? { body: body as BodyInit, duplex: "half" } : {}),
          } as RequestInit);
          const response = await handle(request);
          if (outgoing.destroyed) {
            await response.body?.cancel();
            return;
          }
          outgoing.writeHead(response.status, Object.fromEntries(response.headers));
          if (!response.body) { outgoing.end(); return; }
          const reader = response.body.getReader();
          const cancel = () => { void reader.cancel().catch(() => {}); };
          outgoing.on("close", cancel);
          try {
            while (!outgoing.destroyed) {
              const chunk = await reader.read();
              if (chunk.done) break;
              if (!outgoing.write(chunk.value)) {
                await once(outgoing, "drain", { signal: AbortSignal.timeout(5000) });
              }
            }
            if (!outgoing.destroyed) outgoing.end();
          } finally {
            outgoing.off("close", cancel);
            await reader.cancel().catch(() => {});
            reader.releaseLock();
          }
        } catch {
          if (!outgoing.destroyed) {
            if (!outgoing.headersSent) outgoing.writeHead(500);
            outgoing.end();
          }
        }
      });
    },
  };
}
