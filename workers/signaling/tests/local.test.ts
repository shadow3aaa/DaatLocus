import { expect, test } from "bun:test";
import { mkdtemp, writeFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { hash, type Proof } from "../src/auth";
const worker = process.env.DAAT_LOCUS_TEST_RELAY_WORKER === "1";
const encode = new TextEncoder();
const base64 = (bytes: ArrayBuffer | Uint8Array) => Buffer.from(bytes).toString("base64url");
async function sign(key: CryptoKeyPair, purpose: string, payload: unknown): Promise<Proof> {
  const bytes = encode.encode(JSON.stringify(payload)); const prefix = encode.encode(`DaatLocus.Stage.v1\0${purpose}\0`);
  const body = new Uint8Array(prefix.length + bytes.length); body.set(prefix); body.set(bytes, prefix.length);
  return { payload: base64(bytes), signature: base64(await crypto.subtle.sign("Ed25519", key.privateKey, body)) };
}
const inboxes=new WeakMap<WebSocket,{queue:Record<string,unknown>[];waiters:Array<(value:Record<string,unknown>)=>void>;closed:boolean}>();
function next(socket:WebSocket):Promise<Record<string,unknown>> {
  let inbox=inboxes.get(socket);
  if(!inbox){inbox={queue:[],waiters:[],closed:false};inboxes.set(socket,inbox);const value=inbox;
    socket.binaryType="arraybuffer";
    socket.addEventListener("message",event=>{const item=typeof event.data==="string"?JSON.parse(event.data):{type:"binary",bytes:new Uint8Array(event.data)};const waiter=value.waiters.shift();if(waiter)waiter(item);else value.queue.push(item);});
    socket.addEventListener("close",()=>{value.closed=true;while(value.waiters.length)value.waiters.shift()!({type:"closed_socket"});});
  }
  if(inbox.queue.length)return Promise.resolve(inbox.queue.shift()!);
  if(inbox.closed)return Promise.resolve({type:"closed_socket"});
  const value=inbox;
  return new Promise((resolve,reject)=>{const deliver=(item:Record<string,unknown>)=>{clearTimeout(timer);resolve(item);};
    const timer=setTimeout(()=>{value.waiters=value.waiters.filter(waiter=>waiter!==deliver);reject(Error("Relay response timed out"));},5000);value.waiters.push(deliver);
  });
}
function binary(channel:string,kind:number,body:Uint8Array){
  const id=Uint8Array.from(Buffer.from(channel.replaceAll("-",""),"hex"));
  const frame=new Uint8Array(18+body.length);frame.set([2,kind]);frame.set(id,2);frame.set(body,18);return frame;
}

test("real relay authenticates, forwards bounded channels and rejects execution authority", async () => {
  const authority = await crypto.subtle.generateKey("Ed25519", true, ["sign", "verify"]) as CryptoKeyPair;
  const authorityBytes = new Uint8Array(await crypto.subtle.exportKey("raw", authority.publicKey));
  const root = { group_id: await hash(authorityBytes), public_key: base64(authorityBytes) };
  const directory = await mkdtemp(resolve(tmpdir(),"dl-relay-test-"));
  const reservation=Bun.serve({hostname:"127.0.0.1",port:0,fetch:()=>new Response()});const port=reservation.port;await reservation.stop(true);
  const config = resolve(directory, "wrangler.json");
  await writeFile(config, JSON.stringify({ name: "dl-local-signal-test", main: resolve("src/index.ts"), compatibility_date: "2026-10-01", durable_objects: { bindings: [{ name: "GROUPS", class_name: "SignalGroup" },{name:"ACCOUNTS",class_name:"AccountRoom"}] }, migrations: [{ tag: "v1", new_sqlite_classes: ["SignalGroup"] },{tag:"v2",new_sqlite_classes:["AccountRoom"]}], vars: { GROUP_ROOTS: JSON.stringify({ [root.group_id]: root.public_key }), REVOKED_STAGES: "[]" } }));
  const environment = Object.fromEntries(Object.entries(process.env).filter(([name]) => !name.startsWith("CLOUDFLARE_") && !name.startsWith("CF_") && !name.startsWith("WRANGLER_")));
  const command=worker?[process.env.DAAT_LOCUS_TEST_NODE??"node","node_modules/wrangler/bin/wrangler.js","dev","--local","--config",config,"--ip","127.0.0.1","--port",String(port),"--persist-to",resolve(directory,"state")]:[process.execPath,"run",resolve("server.ts")];
  const child = Bun.spawn({cmd:command,env:{...environment,PORT:String(port),RELAY_DATABASE:resolve(directory,"relay.sqlite"),GROUP_ROOTS:JSON.stringify({[root.group_id]:root.public_key}),REVOKED_STAGES:"[]",WRANGLER_SEND_METRICS:"false"},stdout:"pipe",stderr:"pipe"});

  let diagnostic = "";
  const readers: ReadableStreamDefaultReader<Uint8Array>[] = [];
  const drain = async (stream: ReadableStream<Uint8Array>) => { const reader = stream.getReader(); readers.push(reader); try { while (true) { const chunk = await reader.read(); if (chunk.done) break; diagnostic = (diagnostic + new TextDecoder().decode(chunk.value)).slice(-8192); } } finally { reader.releaseLock(); } };
  const draining = Promise.all([drain(child.stdout), drain(child.stderr)]);
  const sockets: WebSocket[] = [];
  try {
    let ready = false;
    for (let i = 0; i < 50; i++) { try { await fetch(`http://127.0.0.1:${port}/health`, { signal: AbortSignal.timeout(300) }); ready = true; break; } catch { await Bun.sleep(100); } }
    if (!ready) throw new Error(`Local Worker did not start: ${diagnostic}`);
    const join = async (can_enroll=false) => {
      const key = await crypto.subtle.generateKey("Ed25519", true, ["sign", "verify"]) as CryptoKeyPair;
      const device = { stage_id: crypto.randomUUID(), public_key: base64(await crypto.subtle.exportKey("raw", key.publicKey)), tls_sha256: "a".repeat(64) };
      const now = Date.now(); const enrollment = { root, chain: [await sign(authority, "membership", { version: 1, group_id: root.group_id, issuer: null, device, join_id: crypto.randomUUID(), issued_at_ms: now, expires_at_ms: now + 60000, can_enroll })] };
      const socket = new WebSocket(`ws://127.0.0.1:${port}/connect/${root.group_id}`); sockets.push(socket);
      const challenge = await next(socket); expect(challenge.type).toBe("challenge");
      const registered = next(socket);
      socket.send(JSON.stringify({ type: "register", protocol:2, enrollment, proof: await sign(key, "relay-register", { version: 2, group_id: root.group_id, stage_id: device.stage_id, challenge: challenge.nonce, expires_at_ms: challenge.expires_at_ms }) }));
      expect((await registered).type).toBe("registered"); return { key, device, socket, enrollment, challenge: challenge.nonce as string };
    };
    const a = await join(); const presence = next(a.socket); const b = await join(true);
    expect((await presence).type).toBe("presence");
    // A replacement must not broadcast to its closing predecessor or include
    // it as an online peer. This happens during ordinary device reconnects.
    const oldSocket=b.socket;
    const oldClosed=new Promise<void>(resolve=>oldSocket.addEventListener("close",()=>resolve(),{once:true}));
    const replacement=new WebSocket(`ws://127.0.0.1:${port}/connect/${root.group_id}`);sockets.push(replacement);
    const reconnectChallenge=await next(replacement);
    const reconnected=next(replacement);const replacementPresence=next(a.socket);
    replacement.send(JSON.stringify({type:"register",protocol:2,enrollment:b.enrollment,proof:await sign(b.key,"relay-register",{version:2,group_id:root.group_id,stage_id:b.device.stage_id,challenge:reconnectChallenge.nonce,expires_at_ms:reconnectChallenge.expires_at_ms})}));
    expect((await reconnected).peers).toEqual([a.device.stage_id]);
    await Promise.race([oldClosed,Bun.sleep(5000).then(()=>{throw Error("Old relay connection remained open");})]);
    expect((await replacementPresence).online).toBe(true);
    b.socket=replacement;
    const [opener,recipient]=a.device.stage_id<b.device.stage_id?[a,b]:[b,a];
    const channel=crypto.randomUUID();const incoming=next(recipient.socket);
    opener.socket.send(JSON.stringify({type:"open",channel,target:recipient.device.stage_id}));
    expect(await incoming).toEqual({type:"incoming",channel,source:opener.device.stage_id});
    const opened=next(opener.socket);recipient.socket.send(JSON.stringify({type:"accept",channel}));
    expect(await opened).toEqual({type:"opened",channel});
    const payload=Uint8Array.from([0,255,17,42]);const delivered=next(recipient.socket);
    opener.socket.send(binary(channel,0,payload));
    expect((await delivered).bytes).toEqual(binary(channel,0,payload));
    const credited=next(opener.socket);const credit=new Uint8Array(4);new DataView(credit.buffer).setUint32(0,payload.length);
    recipient.socket.send(binary(channel,1,credit));expect((await credited).bytes).toEqual(binary(channel,1,credit));
    const closedLeft=next(opener.socket);const closedRight=next(recipient.socket);
    opener.socket.send(JSON.stringify({type:"close",channel}));
    expect(await closedLeft).toEqual({type:"closed",channel});expect(await closedRight).toEqual({type:"closed",channel});
    const revokedRecord={enrollment:b.enrollment,proof:await sign(b.key,"account-revocation",{version:1,group_id:root.group_id,issuer:b.device.stage_id,target:a.device.stage_id,issued_at_ms:Date.now(),nonce:crypto.randomUUID()})};
    const revocationNotice=next(b.socket);
    const removed=new Promise<void>(resolve=>a.socket.addEventListener("close",()=>resolve(),{once:true}));
    b.socket.send(JSON.stringify({type:"revoke",record:revokedRecord}));
    expect((await revocationNotice).type).toBe("revocations");
    await Promise.race([removed,Bun.sleep(5000).then(()=>{throw new Error("Revoked relay socket stayed connected");})]);
    const c=await join();
    expect((await next(c.socket)).type).toBe("revocations");
    // Re-registration of the removed identity is refused even with its original valid proof.
    const denied=new WebSocket(`ws://127.0.0.1:${port}/connect/${root.group_id}`);sockets.push(denied);
    const deniedChallenge=await next(denied);
    const deniedClosed=new Promise<void>(resolve=>denied.addEventListener("close",()=>resolve(),{once:true}));
    denied.send(JSON.stringify({type:"register",protocol:2,enrollment:a.enrollment,proof:await sign(a.key,"relay-register",{version:2,group_id:root.group_id,stage_id:a.device.stage_id,challenge:deniedChallenge.nonce,expires_at_ms:deniedChallenge.expires_at_ms})}));
    await Promise.race([deniedClosed,Bun.sleep(5000).then(()=>{throw new Error("Revoked device registered again");})]);
    const nameHash = await hash(encode.encode("DaatLocus.AccountLookup.v1/alice"));
    async function accountParticipant(publishing: boolean) {
      const key=await crypto.subtle.generateKey("Ed25519",true,["sign","verify"]) as CryptoKeyPair;
      const device={stage_id:crypto.randomUUID(),public_key:base64(await crypto.subtle.exportKey("raw",key.publicKey)),tls_sha256:"a".repeat(64)};
      const enrollment=publishing?{root,chain:[await sign(authority,"membership",{version:1,group_id:root.group_id,issuer:null,device,join_id:crypto.randomUUID(),issued_at_ms:Date.now(),expires_at_ms:Date.now()+300000,can_enroll:true})]}:null;
      const socket=new WebSocket(`ws://127.0.0.1:${port}/accounts/${nameHash}`);sockets.push(socket);
      const challenge=await next(socket);const registered=next(socket);
      socket.send(JSON.stringify({type:"register",device,enrollment,label:publishing?"system-name":"",proof:await sign(key,"account-rendezvous",{version:1,challenge:challenge.nonce,expires_at_ms:challenge.expires_at_ms,name_hash:nameHash,device,account_id:publishing?root.group_id:null})}));
      return {socket,device,registered:await registered};
    }
    const accountServer=await accountParticipant(true);const accountClient=await accountParticipant(false);
    expect((accountClient.registered.peers as Array<{device:{stage_id:string}}>)[0].device.stage_id).toBe(accountServer.device.stage_id);
    const exchange=crypto.randomUUID();const accountDelivered=next(accountServer.socket);
    accountClient.socket.send(JSON.stringify({type:"forward",target:accountServer.device.stage_id,nonce:exchange,packet:{kind:"start",login:{request:"opaque-protocol-message"}}}));
    const forwarded=await accountDelivered;expect(forwarded.source).toEqual(accountClient.device);expect(forwarded.nonce).toBe(exchange);
    const accepted=next(accountClient.socket);accountServer.socket.send(JSON.stringify({type:"forward",target:accountClient.device.stage_id,nonce:exchange,packet:{kind:"accepted",payload:"encrypted-device-result"}}));
    expect((await accepted).packet).toEqual({kind:"accepted",payload:"encrypted-device-result"});
    const invalidDirection=new Promise<void>(resolve=>accountClient.socket.addEventListener("close",()=>resolve(),{once:true}));
    accountClient.socket.send(JSON.stringify({type:"forward",target:accountServer.device.stage_id,nonce:exchange,packet:{kind:"accepted",payload:"forged"}}));
    await Promise.race([invalidDirection,Bun.sleep(5000).then(()=>{throw Error("Client forged a server login result");})]);
    // A receiver grants no credit for these bytes. The fifth chunk exceeds
    // the fixed window and closes only the offending device/channel.
    const d=await join();const e=await join();
    const [sender,receiver]=d.device.stage_id<e.device.stage_id?[d,e]:[e,d];
    async function kind(socket:WebSocket,type:string){for(let i=0;i<16;i++){const value=await next(socket);if(value.type===type)return value;}throw Error(`Missing ${type}`);}
    const bounded=crypto.randomUUID();const offered=kind(receiver.socket,"incoming");
    sender.socket.send(JSON.stringify({type:"open",channel:bounded,target:receiver.device.stage_id}));await offered;
    const channelReady=kind(sender.socket,"opened");receiver.socket.send(JSON.stringify({type:"accept",channel:bounded}));await channelReady;
    for(let i=0;i<4;i++){
      const data=kind(receiver.socket,"binary");sender.socket.send(binary(bounded,0,new Uint8Array(16384).fill(42)));
      expect(((await data).bytes as Uint8Array).slice(18)).toEqual(new Uint8Array(16384).fill(42));
    }
    const senderClosed=kind(sender.socket,"closed_socket");const receiverClosed=kind(receiver.socket,"closed");
    sender.socket.send(binary(bounded,0,Uint8Array.of(1)));
    await senderClosed;expect((await receiverClosed).channel).toBe(bounded);
    const rejected = new Promise<void>((resolve) => c.socket.addEventListener("close", () => resolve(), { once: true }));
    c.socket.send(JSON.stringify({ type: "core", operation: {kind:"claim",head:{sequence:0,hash:"0".repeat(64)}} }));
    await Promise.race([rejected, Bun.sleep(5000).then(() => { throw new Error("Worker accepted execution coordination"); })]);
  } catch (error) {
    console.error(diagnostic); throw error;
  } finally {
    for (const socket of sockets) socket.close();
    child.kill(); await Promise.race([child.exited, Bun.sleep(2000)]);
    await Promise.race([Promise.all(readers.map((reader) => reader.cancel().catch(() => {}))), Bun.sleep(1000)]);
    await Promise.race([draining, Bun.sleep(1000)]);
    await rm(directory,{recursive:true,force:true});
  }
}, 30000);
