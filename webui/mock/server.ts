/**
 * Bun-native mock of the DaatLocus daemon HTTP API for WebUI development.
 * Start with `bun mock/server.ts` (or `bun run mock`), then run the WebUI with
 * `DAAT_LOCUS_DAEMON_URL=http://127.0.0.1:45826 bun run dev` (or `bun run dev:mock`).
 */
import { resetStore } from "./fixtures";
import { handle } from "./routes";
import { MOCK_PORT } from "./state";

resetStore();

const server = Bun.serve({
  port: MOCK_PORT,
  hostname: "127.0.0.1",
  fetch: request => handle(request),
});

console.log(`[mock] DaatLocus mock daemon listening on http://${server.hostname}:${server.port}`);
console.log("[mock] health: GET /__mock/health · reset: POST /__mock/reset");
