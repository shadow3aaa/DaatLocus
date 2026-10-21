import { bytes, hash, membership, registration, verify, type Device, type Enrollment, type Proof, type Root } from "./auth";
import type { RelayContext, RelaySocket, RelayEnv } from "./runtime";
export const WINDOW = 64 * 1024;
export const CHUNK = 16 * 1024;
interface Channel { id:string; peer:string; credit:number; pending:number; ready:boolean }
interface Attachment {
  group: string; challenge: string; deadline: number; rootKey?: string;
  device?: Device; expiry?: number; issuers?: string[]; window: number; messages: number;
  channels: Channel[]; protocol: number;
}
function roots(env: RelayEnv): Record<string, string> {
  const value = JSON.parse(env.GROUP_ROOTS ?? "{}") as Record<string, string>;
  if (Object.keys(value).length > 64) throw new Error("Group budget exceeded");
  return value;
}
function revoked(env: RelayEnv): Set<string> {
  const value = JSON.parse(env.REVOKED_STAGES || "[]") as string[];
  if (!Array.isArray(value) || value.length > 1024) throw new Error("Revocation budget exceeded");
  return new Set(value);
}
function require(value:unknown):asserts value { if(!value)throw Error("Invalid relay frame"); }
const uuid = /^[0-9a-f]{8}(-[0-9a-f]{4}){3}-[0-9a-f]{12}$/;
export class RelayGroup {
  private removed = new Set<string>();
  private removals: Array<{enrollment:Enrollment;proof:Proof}> = [];
  constructor(private ctx: RelayContext, private env: RelayEnv) {
    ctx.blockConcurrencyWhile(async () => {
      this.removals = await ctx.storage.get<Array<{enrollment:Enrollment;proof:Proof}>>("revocations") ?? [];
      for (const item of this.removals) { const data = JSON.parse(atob(item.proof.payload.replace(/-/g,"+").replace(/_/g,"/"))) as { target:string }; this.removed.add(data.target); }
    });
  }
  private authorized(socket: RelaySocket, now: number): Attachment | undefined {
    if (socket.readyState !== 1) return;
    const attachment = socket.deserializeAttachment() as Attachment | undefined;
    if (attachment?.protocol !== 2 || !attachment.device || !attachment.rootKey || !attachment.expiry || attachment.expiry <= now) return;
    const allowed = roots(this.env);
    if (Object.keys(allowed).length && allowed[attachment.group] !== attachment.rootKey) return;
    const removed = new Set([...revoked(this.env), ...this.removed]);
    if (removed.has(attachment.device.stage_id) || attachment.issuers?.some((issuer) => removed.has(issuer))) return;
    return attachment;
  }
  async connect(server: RelaySocket, group: string): Promise<void> {
    const now = Date.now(); const challenge = crypto.randomUUID();
    server.serializeAttachment({ group, challenge, deadline: now + 30000, window: now, messages: 0, channels: [], protocol: 2 } satisfies Attachment);
    await this.ctx.storage.setAlarm(now + 30000);
    server.send(JSON.stringify({ type: "challenge", nonce: challenge, expires_at_ms: now + 30000, protocol: 2 }));
  }
  async webSocketMessage(socket: RelaySocket, message: string | ArrayBuffer): Promise<void> {
    try {
      if (typeof message !== "string") { this.forwardBinary(socket, message); return; }
      if ( new TextEncoder().encode(message).length > 64 * 1024) throw new Error("Message budget");
      const now = Date.now(); let state = socket.deserializeAttachment() as Attachment;
      if (now - state.window > 1000) { state.window = now; state.messages = 0; }
      if (++state.messages > 64) throw new Error("Rate budget");
      socket.serializeAttachment(state);
      const input = JSON.parse(message) as { type: string; enrollment?: Enrollment; proof?: Proof; target?: string; record?:{enrollment:Enrollment;proof:Proof}; protocol?:number; channel?:string };
      if (!state.device) {
        if (input.type !== "register" || input.protocol !== 2 || now >= state.deadline || !input.enrollment || !input.proof) throw new Error("Registration required");
        const root: Root = input.enrollment.root;
        const allowed = roots(this.env);
        if (root.group_id !== state.group || await hash(bytes(root.public_key,32)) !== state.group || Object.keys(allowed).length && allowed[state.group] !== root.public_key) throw new Error("Invalid rendezvous namespace");
        const member = await membership(input.enrollment, root, new Set([...revoked(this.env),...this.removed]), now);
        await registration(input.proof, member, state.group, state.challenge, now,state.deadline);
        const issuers = [] as string[];
        for (const proof of input.enrollment.chain) {
          const payload = JSON.parse(atob(proof.payload.replace(/-/g, "+").replace(/_/g, "/"))) as { device: Device };
          issuers.push(payload.device.stage_id);
        }
        state = { ...state, rootKey: root.public_key, device: member.device, expiry: member.expires_at_ms, issuers };
        for (const old of this.ctx.getWebSockets()) {
          if (old !== socket && old.readyState === 1 && (old.deserializeAttachment() as Attachment)?.device?.stage_id === member.device.stage_id) { this.closeChannels(old); old.close(1000, "Connection replaced"); }
        }
        socket.serializeAttachment(state);
        socket.send(JSON.stringify({ type: "registered", protocol: 2, peers: this.online(state.group, now).filter((peer) => peer !== member.device.stage_id) }));
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
          else if((peer.deserializeAttachment() as Attachment)?.group===access.group){this.closeChannels(peer);peer.close(1008,"Authorization revoked");}
        }
        return;
      }
      this.control(socket, input, now);

    } catch { this.closeChannels(socket); socket.close(1008, "Relay rejected"); }
  }
  private online(group: string, now: number): string[] {
    return this.ctx.getWebSockets().flatMap((socket) => { const value = this.authorized(socket, now); return value?.group === group && value.device ? [value.device.stage_id] : []; });
  }
  private presence(group: string, stage: string, online: boolean, exclude: RelaySocket, now: number): void {
    for (const socket of this.ctx.getWebSockets()) if (socket !== exclude && this.authorized(socket, now)?.group === group) {
      try { socket.send(JSON.stringify({ type: "presence", stage_id: stage, online })); }
      catch { socket.close(1011, "Connection failed"); }
    }
  }
  webSocketClose(socket: RelaySocket): void {
    this.closeChannels(socket);
    const state = socket.deserializeAttachment() as Attachment | undefined;
    if (state?.device && !this.ctx.getWebSockets().some((other) => other !== socket && this.authorized(other, Date.now())?.device?.stage_id === state.device?.stage_id)) this.presence(state.group, state.device.stage_id, false, socket, Date.now());
    socket.close(1000, "Closed");
  }
  webSocketError(socket: RelaySocket): void { this.closeChannels(socket); socket.close(1011, "Connection failed"); }
  async alarm(): Promise<void> {
    const now = Date.now(); let next = Number.MAX_SAFE_INTEGER;
    for (const socket of this.ctx.getWebSockets()) {
      const state = socket.deserializeAttachment() as Attachment | undefined;
      const deadline = state?.device ? state.expiry : state?.deadline;
      if (!deadline || deadline <= now || (state?.device && !this.authorized(socket, now))) { this.closeChannels(socket); socket.close(1008, "Authorization expired"); }
      else next = Math.min(next, deadline);
    }
    if (next !== Number.MAX_SAFE_INTEGER) await this.ctx.storage.setAlarm(next);
  }
  private endpoint(group:string, device:string, now:number):RelaySocket|undefined {
    return this.ctx.getWebSockets().find(socket=>{const state=this.authorized(socket,now);return state?.group===group&&state.device?.stage_id===device;});
  }
  private control(socket:RelaySocket,input:{type:string;channel?:string;target?:string},now:number) {
    const state=this.authorized(socket,now);require(state?.device&&input.channel&&uuid.test(input.channel));
    const id=input.channel;
    if(input.type==="open") {
      require(input.target&&uuid.test(input.target)&&input.target>state.device.stage_id&&state.channels.length<32);
      require(!state.channels.some(item=>item.id===id||item.peer===input.target));
      const other=this.endpoint(state.group,input.target,now);
      if(!other){socket.send(JSON.stringify({type:"closed",channel:id}));return;}
      const remote=other.deserializeAttachment() as Attachment;
      require(remote.channels.length<32&&!remote.channels.some(item=>item.id===id||item.peer===state.device!.stage_id));
      state.channels.push({id,peer:input.target,credit:WINDOW,pending:0,ready:false});
      remote.channels.push({id,peer:state.device.stage_id,credit:WINDOW,pending:0,ready:false});
      socket.serializeAttachment(state);other.serializeAttachment(remote);
      other.send(JSON.stringify({type:"incoming",channel:id,source:state.device.stage_id}));return;
    }
    const channel=state.channels.find(item=>item.id===id);
    if(!channel){socket.send(JSON.stringify({type:"closed",channel:id}));return;}
    const other=this.endpoint(state.group,channel.peer,now);
    if(!other){this.closeChannel(socket,id);return;}
    const remote=other.deserializeAttachment() as Attachment;
    const opposite=remote.channels.find(item=>item.id===id&&item.peer===state.device!.stage_id);
    require(opposite);
    if(input.type==="accept") {
      // Only the recipient of open can accept: stable device order chooses opener.
      require(state.device.stage_id>channel.peer&&!channel.ready&&!opposite.ready);
      channel.ready=true;opposite.ready=true;
      socket.serializeAttachment(state);other.serializeAttachment(remote);
      other.send(JSON.stringify({type:"opened",channel:id}));return;
    }
    require(input.type==="close");this.closeChannel(socket,id);
  }
  private forwardBinary(socket:RelaySocket,message:ArrayBuffer) {
    const now=Date.now();const state=this.authorized(socket,now);require(state?.device);
    const data=new Uint8Array(message);require(data.length>=18&&data[0]===2&&(data[1]===0||data[1]===1));
    const hex=Array.from(data.slice(2,18),byte=>byte.toString(16).padStart(2,"0")).join("");
    const id=`${hex.slice(0,8)}-${hex.slice(8,12)}-${hex.slice(12,16)}-${hex.slice(16,20)}-${hex.slice(20)}`;
    const channel=state.channels.find(item=>item.id===id);if(!channel){socket.send(JSON.stringify({type:"closed",channel:id}));return;}require(channel.ready);
    const other=this.endpoint(state.group,channel.peer,now);require(other);
    const remote=other.deserializeAttachment() as Attachment;
    const opposite=remote.channels.find(item=>item.id===id&&item.peer===state.device!.stage_id);require(opposite?.ready);
    if(data[1]===0){const length=data.length-18;require(length>0&&length<=CHUNK&&length<=channel.credit);
      channel.credit-=length;opposite.pending+=length;require(opposite.pending<=WINDOW);
    }else{require(data.length===22);const amount=new DataView(message).getUint32(18);
      require(amount>0&&amount<=channel.pending&&opposite.credit+amount<=WINDOW);
      channel.pending-=amount;opposite.credit+=amount;
    }
    socket.serializeAttachment(state);other.serializeAttachment(remote);other.send(message);
  }
  private closeChannel(socket:RelaySocket,id:string) {
    const state=socket.deserializeAttachment() as Attachment;
    const channel=state.channels?.find(item=>item.id===id);if(!channel)return;
    state.channels=state.channels.filter(item=>item.id!==id);socket.serializeAttachment(state);
    if(socket.readyState===1)socket.send(JSON.stringify({type:"closed",channel:id}));
    const other=this.ctx.getWebSockets().find(peer=>peer!==socket&&(peer.deserializeAttachment() as Attachment)?.device?.stage_id===channel.peer);
    if(other){const remote=other.deserializeAttachment() as Attachment;
      remote.channels=remote.channels.filter(item=>!(item.id===id&&item.peer===state.device?.stage_id));other.serializeAttachment(remote);
      if(other.readyState===1)other.send(JSON.stringify({type:"closed",channel:id}));}
  }
  private closeChannels(socket:RelaySocket){
    const state=socket.deserializeAttachment() as Attachment|undefined;
    for(const channel of state?.channels??[])this.closeChannel(socket,channel.id);
  }
}
