import { DurableObject } from "cloudflare:workers";
import {AccountRoom} from "./accounts";
export {AccountRoom};
import { bytes, hash, membership, negotiation, registration, verify, type Device, type Enrollment, type Proof, type Root } from "./auth";
interface Env { GROUPS: DurableObjectNamespace<SignalGroup>; ACCOUNTS: DurableObjectNamespace<AccountRoom>; GROUP_ROOTS?: string; REVOKED_STAGES: string }
interface Attachment {
  group: string; challenge: string; deadline: number; rootKey?: string;
  device?: Device; expiry?: number; issuers?: string[]; window: number; messages: number;
}
function roots(env: Env): Record<string, string> {
  const value = JSON.parse(env.GROUP_ROOTS ?? "{}") as Record<string, string>;
  if (Object.keys(value).length > 64) throw new Error("Group budget exceeded");
  return value;
}
function revoked(env: Env): Set<string> {
  const value = JSON.parse(env.REVOKED_STAGES) as string[];
  if (!Array.isArray(value) || value.length > 1024) throw new Error("Revocation budget exceeded");
  return new Set(value);
}
export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const account=new URL(request.url).pathname.match(/^\/accounts\/([0-9a-f]{64})$/);
    if(request.method==="GET"&&account&&request.headers.get("Upgrade")?.toLowerCase()==="websocket")return env.ACCOUNTS.get(env.ACCOUNTS.idFromName(account[1])).fetch(request);
    const match = new URL(request.url).pathname.match(/^\/connect\/([0-9a-f]{64})$/);
    if (request.method !== "GET" || !match || request.headers.get("Upgrade")?.toLowerCase() !== "websocket") return new Response("Not found", { status: 404 });
    const allowed = roots(env);
    if (Object.keys(allowed).length && !allowed[match[1]]) return new Response("Forbidden", { status: 403 });
    return env.GROUPS.get(env.GROUPS.idFromName(match[1])).fetch(request);
  },
};
export class SignalGroup extends DurableObject<Env> {
  private removed = new Set<string>();
  private removals: Array<{enrollment:Enrollment;proof:Proof}> = [];
  constructor(ctx: DurableObjectState, env: Env) {
    super(ctx, env);
    ctx.blockConcurrencyWhile(async () => {
      this.removals = await ctx.storage.get<Array<{enrollment:Enrollment;proof:Proof}>>("revocations") ?? [];
      for (const item of this.removals) { const data = JSON.parse(atob(item.proof.payload.replace(/-/g,"+").replace(/_/g,"/"))) as { target:string }; this.removed.add(data.target); }
    });
  }
  private authorized(socket: WebSocket, now: number): Attachment | undefined {
    if (socket.readyState !== WebSocket.OPEN) return;
    const attachment = socket.deserializeAttachment() as Attachment | undefined;
    if (!attachment?.device || !attachment.rootKey || !attachment.expiry || attachment.expiry <= now) return;
    const allowed = roots(this.env);
    if (Object.keys(allowed).length && allowed[attachment.group] !== attachment.rootKey) return;
    const removed = new Set([...revoked(this.env), ...this.removed]);
    if (removed.has(attachment.device.stage_id) || attachment.issuers?.some((issuer) => removed.has(issuer))) return;
    return attachment;
  }
  async fetch(request: Request): Promise<Response> {
    const group = new URL(request.url).pathname.split("/").pop()!;
    if (this.ctx.getWebSockets().length >= 64) return new Response("Busy", { status: 503 });
    const pair = new WebSocketPair(); const client = pair[0]; const server = pair[1];
    const now = Date.now(); const challenge = crypto.randomUUID();
    server.serializeAttachment({ group, challenge, deadline: now + 30000, window: now, messages: 0 } satisfies Attachment);
    this.ctx.acceptWebSocket(server);
    await this.ctx.storage.setAlarm(now + 30000);
    server.send(JSON.stringify({ type: "challenge", nonce: challenge, expires_at_ms: now + 30000 }));
    return new Response(null, { status: 101, webSocket: client });
  }
  async webSocketMessage(socket: WebSocket, message: string | ArrayBuffer): Promise<void> {
    try {
      if (typeof message !== "string" || new TextEncoder().encode(message).length > 64 * 1024) throw new Error("Message budget");
      const now = Date.now(); let state = socket.deserializeAttachment() as Attachment;
      if (now - state.window > 1000) { state.window = now; state.messages = 0; }
      if (++state.messages > 64) throw new Error("Rate budget");
      socket.serializeAttachment(state);
      const input = JSON.parse(message) as { type: string; enrollment?: Enrollment; proof?: Proof; target?: string; record?:{enrollment:Enrollment;proof:Proof} };
      if (!state.device) {
        if (input.type !== "register" || now >= state.deadline || !input.enrollment || !input.proof) throw new Error("Registration required");
        const root: Root = input.enrollment.root;
        const allowed = roots(this.env);
        if (root.group_id !== state.group || await hash(bytes(root.public_key,32)) !== state.group || Object.keys(allowed).length && allowed[state.group] !== root.public_key) throw new Error("Invalid rendezvous namespace");
        const member = await membership(input.enrollment, root, new Set([...revoked(this.env),...this.removed]), now);
        await registration(input.proof, member, state.group, state.challenge, now);
        const issuers = [] as string[];
        for (const proof of input.enrollment.chain) {
          const payload = JSON.parse(atob(proof.payload.replace(/-/g, "+").replace(/_/g, "/"))) as { device: Device };
          issuers.push(payload.device.stage_id);
        }
        state = { ...state, rootKey: root.public_key, device: member.device, expiry: member.expires_at_ms, issuers };
        for (const old of this.ctx.getWebSockets()) {
          if (old !== socket && old.readyState === WebSocket.OPEN && (old.deserializeAttachment() as Attachment)?.device?.stage_id === member.device.stage_id) old.close(1000, "Connection replaced");
        }
        socket.serializeAttachment(state);
        socket.send(JSON.stringify({ type: "registered", peers: this.online(state.group, now).filter((peer) => peer !== member.device.stage_id) }));
        for (let start=0;start<this.removals.length;start+=8) socket.send(JSON.stringify({type:"revocations",records:this.removals.slice(start,start+8)}));
        this.presence(state.group, member.device.stage_id, true, socket, now);
        return;
      }
      const access = this.authorized(socket, now);
      if (!access?.device) throw new Error("Authorization expired");
      if (input.type === "revoke" && input.record) {
        const item=input.record;
        const root:Root={group_id:access.group,public_key:access.rootKey!};
        const proposed=JSON.parse(atob(item.proof.payload.replace(/-/g,"+").replace(/_/g,"/"))) as {issued_at_ms:number};
        if(!Number.isSafeInteger(proposed.issued_at_ms) || proposed.issued_at_ms<0 || proposed.issued_at_ms>now+60000)throw new Error("Invalid revocation time");
        const issuer=await membership(item.enrollment,root,new Set(),proposed.issued_at_ms);
        const change=await verify<{version:number;group_id:string;issuer:string;target:string;issued_at_ms:number;nonce:string}>(item.proof,"account-revocation",issuer.device.public_key);
        if (Object.keys(change).sort().join()!=="group_id,issued_at_ms,issuer,nonce,target,version" || change.version!==1 || !issuer.can_enroll || change.group_id!==access.group || issuer.device.stage_id!==change.issuer || !/^[0-9a-f-]{36}$/.test(change.target) || !Number.isSafeInteger(change.issued_at_ms) || change.issued_at_ms>now+60000 || !/^[0-9a-f]{8}(-[0-9a-f]{4}){3}-[0-9a-f]{12}$/.test(change.nonce)) throw new Error("Invalid revocation");
        const ancestor=JSON.parse(atob(item.enrollment.chain[0].payload.replace(/-/g,"+").replace(/_/g,"/"))) as {device:Device};
        if(item.enrollment.chain.length>1 && ancestor.device.stage_id===change.target)throw new Error("Account authority cannot be revoked by a child");
        if(!this.removed.has(change.target)){
          await membership(item.enrollment,root,new Set([...revoked(this.env),...this.removed]),Math.min(now,issuer.expires_at_ms-1));
          if(this.removals.length>=1024)throw new Error("Revocation budget");
          this.removals.push(item);this.removed.add(change.target);await this.ctx.storage.put("revocations",this.removals);}
        for(const peer of this.ctx.getWebSockets()) {
          if(this.authorized(peer,now)?.group===access.group)peer.send(JSON.stringify({type:"revocations",records:[item]}));
          else if((peer.deserializeAttachment() as Attachment)?.group===access.group)peer.close(1008,"Authorization revoked");
        }
        return;
      }
      if (input.type !== "signal" || !input.proof) throw new Error("Only negotiation is supported");
      const signal = await negotiation(input.proof, access.device, access.group, now);
      const recipient = this.ctx.getWebSockets().find((peer) => {
        const remote = this.authorized(peer, now);
        return remote?.group === access.group && remote.device?.stage_id === signal.target;
      });
      if (!recipient) { socket.send(JSON.stringify({ type: "unreachable", target: signal.target, nonce: signal.nonce })); return; }
      recipient.send(JSON.stringify({ type: "signal", source: access.device.stage_id, proof: input.proof }));
    } catch (error) { console.debug("Signaling rejected",error instanceof Error ? error.message : "Invalid control"); socket.close(1008, "Signaling rejected"); }
  }
  private online(group: string, now: number): string[] {
    return this.ctx.getWebSockets().flatMap((socket) => { const value = this.authorized(socket, now); return value?.group === group && value.device ? [value.device.stage_id] : []; });
  }
  private presence(group: string, stage: string, online: boolean, exclude: WebSocket, now: number): void {
    for (const socket of this.ctx.getWebSockets()) if (socket !== exclude && this.authorized(socket, now)?.group === group) {
      try { socket.send(JSON.stringify({ type: "presence", stage_id: stage, online })); }
      catch { socket.close(1011, "Connection failed"); }
    }
  }
  webSocketClose(socket: WebSocket): void {
    const state = socket.deserializeAttachment() as Attachment | undefined;
    if (state?.device && !this.ctx.getWebSockets().some((other) => other !== socket && this.authorized(other, Date.now())?.device?.stage_id === state.device?.stage_id)) this.presence(state.group, state.device.stage_id, false, socket, Date.now());
    socket.close(1000, "Closed");
  }
  webSocketError(socket: WebSocket): void { socket.close(1011, "Connection failed"); }
  async alarm(): Promise<void> {
    const now = Date.now(); let next = Number.MAX_SAFE_INTEGER;
    for (const socket of this.ctx.getWebSockets()) {
      const state = socket.deserializeAttachment() as Attachment | undefined;
      const deadline = state?.device ? state.expiry : state?.deadline;
      if (!deadline || deadline <= now || (state?.device && !this.authorized(socket, now))) socket.close(1008, "Authorization expired");
      else next = Math.min(next, deadline);
    }
    if (next !== Number.MAX_SAFE_INTEGER) await this.ctx.storage.setAlarm(next);
  }
}
