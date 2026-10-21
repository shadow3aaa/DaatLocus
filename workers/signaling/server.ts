/** Standalone adapter for the same relay used by Cloudflare Workers.
 * Run behind an HTTPS reverse proxy. No account passwords or Actor data are stored.
 */
import { Database } from "bun:sqlite";
import type { ServerWebSocket } from "bun";
import { RelayGroup } from "./src/group";
import { AccountRelay } from "./src/accounts";
import type { RelayContext, RelaySocket } from "./src/runtime";

const database = new Database(process.env.RELAY_DATABASE ?? "relay.sqlite", { create: true });
database.exec("PRAGMA journal_mode=WAL; CREATE TABLE IF NOT EXISTS relay_values(room TEXT,key TEXT,value TEXT,PRIMARY KEY(room,key));");
const env = { GROUP_ROOTS: process.env.GROUP_ROOTS ?? "{}", REVOKED_STAGES: process.env.REVOKED_STAGES ?? "[]" };
type Room = ReturnType<typeof room>;
type Connection = { room: Room; socket?: Socket; namespace: string };
class Socket implements RelaySocket {
  private attachment: unknown;
  constructor(private socket: ServerWebSocket<Connection>) {}
  get readyState() { return this.socket.readyState; }
  send(data: string | ArrayBuffer) {
    // Protocol credit bounds binary buffers. Also bound control messages and
    // close a transport whose receiver cannot drain its socket.
    if (this.socket.getBufferedAmount() > 4 * 1024 * 1024) {
      this.socket.close(1013, "Receiver stalled");
      return;
    }
    this.socket.send(data);
  }
  close(code?: number, reason?: string) { this.socket.close(code, reason); }
  serializeAttachment(value: unknown) { this.attachment = structuredClone(value); }
  deserializeAttachment() { return structuredClone(this.attachment); }
}
function room(key: string, accounts: boolean) {
  const sockets = new Set<Socket>();
  let pending: Promise<unknown> = Promise.resolve();
  let timer: ReturnType<typeof setTimeout> | undefined;
  const enqueue = (action: () => Promise<unknown> | unknown) => {
    const next = pending.then(action);
    pending = next.catch(() => {});
    return next;
  };
  const context: RelayContext = {
    getWebSockets: () => [...sockets],
    blockConcurrencyWhile: async callback => callback(),
    storage: {
      async get<T>(name: string) {
        const value = database.query("SELECT value FROM relay_values WHERE room=? AND key=?").get(key, name) as { value: string } | null;
        return value ? JSON.parse(value.value) as T : undefined;
      },
      async put<T>(name: string, value: T) {
        database.query("INSERT INTO relay_values VALUES(?,?,?) ON CONFLICT(room,key) DO UPDATE SET value=excluded.value").run(key, name, JSON.stringify(value));
      },
      async setAlarm(time: number) {
        clearTimeout(timer);
        timer = setTimeout(() => { void enqueue(() => core.alarm()); }, Math.max(0, time - Date.now()));
      },
    },
  };
  let initialized: Promise<unknown> = Promise.resolve();
  context.blockConcurrencyWhile = callback => {
    const ready = callback(); initialized = ready; return ready;
  };
  const core = accounts ? new AccountRelay(context, env) : new RelayGroup(context, env);
  return { sockets, core, key, dispose:()=>clearTimeout(timer), enqueue: (action: () => Promise<unknown> | unknown) => enqueue(async () => { await initialized; return action(); }) };
}
const rooms = new Map<string, Room>();
let pendingBytes=0;
const server = Bun.serve<Connection>({
  hostname: process.env.RELAY_LISTEN ?? "127.0.0.1",
  port: Number(process.env.PORT ?? 8787),
  fetch(request, server) {
    const path = new URL(request.url).pathname;
    if (request.method === "GET" && path === "/health") return Response.json({ service: "daat-locus-relay", protocol: 2 });
    const match = path.match(/^\/(accounts|connect)\/([0-9a-f]{64})$/);
    if (request.method !== "GET" || !match || request.headers.get("Upgrade")?.toLowerCase()!=="websocket") return new Response("Not found", { status: 404 });
    if (!rooms.has(path)) {
      if (rooms.size >= 1024) return new Response("Busy", { status: 503 });
      rooms.set(path, room(path, match[1] === "accounts"));
    }
    const selected = rooms.get(path)!;
    if (selected.sockets.size >= 64) return new Response("Busy", { status: 503 });
    if (server.upgrade(request, { data: { room: selected, namespace: match[2] } })) return;
    return new Response("WebSocket required", { status: 426 });
  },
  websocket: {
    maxPayloadLength: 64 * 1024,
    backpressureLimit: 4 * 1024 * 1024,
    closeOnBackpressureLimit: true,
    open(socket) {
      const adapter = new Socket(socket); socket.data.socket = adapter;
      const selected = socket.data.room; selected.sockets.add(adapter);
      void selected.enqueue(() => selected.core.connect(adapter, socket.data.namespace)).catch(()=>socket.close(1011,"Relay unavailable"));
    },
    message(socket, message) {
      const selected = socket.data.room;
      const value = typeof message === "string" ? message : Uint8Array.from(message).buffer;
      const length=typeof value==="string"?new TextEncoder().encode(value).length:value.byteLength;
      if(pendingBytes+length>16*1024*1024){socket.close(1013,"Relay busy");return;}
      pendingBytes+=length;
      void selected.enqueue(() => selected.core.webSocketMessage(socket.data.socket!, value)).catch(()=>socket.close(1011,"Relay unavailable")).finally(()=>{pendingBytes-=length;});
    },
    close(socket) {
      const selected = socket.data.room; const adapter = socket.data.socket!;
      void selected.enqueue(() => { selected.sockets.delete(adapter); selected.core.webSocketClose(adapter);if(!selected.sockets.size){selected.dispose();rooms.delete(selected.key);} });
    },
  },
});
console.log(`DL relay listening on ${server.hostname}:${server.port}`);
