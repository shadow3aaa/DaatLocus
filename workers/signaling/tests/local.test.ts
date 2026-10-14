import { expect, test } from "bun:test";
import { mkdir, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import { hash, type Proof } from "../src/auth";
const enabled = process.env.DAAT_LOCUS_TEST_SIGNALING === "1";
const encode = new TextEncoder();
const base64 = (bytes: ArrayBuffer | Uint8Array) => Buffer.from(bytes).toString("base64url");
async function sign(key: CryptoKeyPair, purpose: string, payload: unknown): Promise<Proof> {
  const bytes = encode.encode(JSON.stringify(payload)); const prefix = encode.encode(`DaatLocus.Stage.v1\0${purpose}\0`);
  const body = new Uint8Array(prefix.length + bytes.length); body.set(prefix); body.set(bytes, prefix.length);
  return { payload: base64(bytes), signature: base64(await crypto.subtle.sign("Ed25519", key.privateKey, body)) };
}
function next(socket: WebSocket): Promise<Record<string, unknown>> {
  return new Promise((resolve, reject) => {
    const timeout = setTimeout(() => { cleanup(); reject(new Error("Local signaling timed out")); }, 5000);
    const message = (event: MessageEvent) => { cleanup(); resolve(JSON.parse(String(event.data))); };
    const close = () => { cleanup(); reject(new Error("Local signaling closed")); };
    const cleanup = () => { clearTimeout(timeout); socket.removeEventListener("message", message); socket.removeEventListener("close", close); };
    socket.addEventListener("message", message); socket.addEventListener("close", close);
  });
}
test.skipIf(!enabled)("real local Durable Object authenticates and forwards only online negotiation", async () => {
  const authority = await crypto.subtle.generateKey("Ed25519", true, ["sign", "verify"]) as CryptoKeyPair;
  const authorityBytes = new Uint8Array(await crypto.subtle.exportKey("raw", authority.publicKey));
  const root = { group_id: await hash(authorityBytes), public_key: base64(authorityBytes) };
  const directory = resolve("../../target/stage-signaling-test"); await mkdir(directory, { recursive: true });
  const config = resolve(directory, "wrangler.json");
  await writeFile(config, JSON.stringify({ name: "dl-local-signal-test", main: resolve("src/index.ts"), compatibility_date: "2026-10-01", durable_objects: { bindings: [{ name: "GROUPS", class_name: "SignalGroup" },{name:"ACCOUNTS",class_name:"AccountRoom"}] }, migrations: [{ tag: "v1", new_sqlite_classes: ["SignalGroup"] },{tag:"v2",new_sqlite_classes:["AccountRoom"]}], vars: { GROUP_ROOTS: JSON.stringify({ [root.group_id]: root.public_key }), REVOKED_STAGES: "[]" } }));
  const environment = Object.fromEntries(Object.entries(process.env).filter(([name]) => !name.startsWith("CLOUDFLARE_") && !name.startsWith("CF_") && !name.startsWith("WRANGLER_")));
  const child = Bun.spawn({ cmd: [process.env.DAAT_LOCUS_TEST_NODE ?? "node", "node_modules/wrangler/bin/wrangler.js", "dev", "--local", "--config", config, "--ip", "127.0.0.1", "--port", "45827", "--persist-to", resolve(directory, "state")], env: { ...environment, WRANGLER_SEND_METRICS: "false" }, stdout: "pipe", stderr: "pipe" });
  let diagnostic = "";
  const readers: ReadableStreamDefaultReader<Uint8Array>[] = [];
  const drain = async (stream: ReadableStream<Uint8Array>) => { const reader = stream.getReader(); readers.push(reader); try { while (true) { const chunk = await reader.read(); if (chunk.done) break; diagnostic = (diagnostic + new TextDecoder().decode(chunk.value)).slice(-8192); } } finally { reader.releaseLock(); } };
  const draining = Promise.all([drain(child.stdout), drain(child.stderr)]);
  const sockets: WebSocket[] = [];
  try {
    let ready = false;
    for (let i = 0; i < 50; i++) { try { await fetch("http://127.0.0.1:45827/", { signal: AbortSignal.timeout(300) }); ready = true; break; } catch { await Bun.sleep(100); } }
    if (!ready) throw new Error(`Local Worker did not start: ${diagnostic}`);
    const join = async (can_enroll=false) => {
      const key = await crypto.subtle.generateKey("Ed25519", true, ["sign", "verify"]) as CryptoKeyPair;
      const device = { stage_id: crypto.randomUUID(), public_key: base64(await crypto.subtle.exportKey("raw", key.publicKey)), tls_sha256: "a".repeat(64) };
      const now = Date.now(); const enrollment = { root, chain: [await sign(authority, "membership", { version: 1, group_id: root.group_id, issuer: null, device, join_id: crypto.randomUUID(), issued_at_ms: now, expires_at_ms: now + 60000, can_enroll })] };
      const socket = new WebSocket(`ws://127.0.0.1:45827/connect/${root.group_id}`); sockets.push(socket);
      const challenge = await next(socket); expect(challenge.type).toBe("challenge");
      const registered = next(socket);
      socket.send(JSON.stringify({ type: "register", enrollment, proof: await sign(key, "signal-register", { version: 1, group_id: root.group_id, stage_id: device.stage_id, challenge: challenge.nonce, expires_at_ms: challenge.expires_at_ms }) }));
      expect((await registered).type).toBe("registered"); return { key, device, socket, enrollment, challenge: challenge.nonce as string };
    };
    const a = await join(); const presence = next(a.socket); const b = await join(true);
    expect((await presence).type).toBe("presence");
    // A replacement must not broadcast to its closing predecessor or include
    // it as an online peer. This happens during ordinary device reconnects.
    const oldSocket=b.socket;
    const oldClosed=new Promise<void>(resolve=>oldSocket.addEventListener("close",()=>resolve(),{once:true}));
    const replacement=new WebSocket(`ws://127.0.0.1:45827/connect/${root.group_id}`);sockets.push(replacement);
    const reconnectChallenge=await next(replacement);
    const reconnected=next(replacement);const replacementPresence=next(a.socket);
    replacement.send(JSON.stringify({type:"register",enrollment:b.enrollment,proof:await sign(b.key,"signal-register",{version:1,group_id:root.group_id,stage_id:b.device.stage_id,challenge:reconnectChallenge.nonce,expires_at_ms:reconnectChallenge.expires_at_ms})}));
    expect((await reconnected).peers).toEqual([a.device.stage_id]);
    await Promise.race([oldClosed,Bun.sleep(5000).then(()=>{throw Error("Old signaling connection remained open");})]);
    expect((await replacementPresence).online).toBe(true);
    b.socket=replacement;
    const fingerprint = Array(32).fill("ab").join(":"); const nonce = crypto.randomUUID();
    const proof = await sign(a.key, "rtc-negotiation", { version: 1, group_id: root.group_id, source: a.device.stage_id, target: b.device.stage_id, generation: 1, nonce, expires_at_ms: Date.now() + 30000, description: { type: "offer", sdp: `v=0\r\na=fingerprint:sha-256 ${fingerprint}\r\n` }, fingerprint });
    const delivered = next(b.socket); a.socket.send(JSON.stringify({ type: "signal", proof }));
    const result = await delivered; expect(result.type).toBe("signal"); expect(result.source).toBe(a.device.stage_id); expect(result.proof).toEqual(proof);
    const revokedRecord={enrollment:b.enrollment,proof:await sign(b.key,"account-revocation",{version:1,group_id:root.group_id,issuer:b.device.stage_id,target:a.device.stage_id,issued_at_ms:Date.now(),nonce:crypto.randomUUID()})};
    const revocationNotice=next(b.socket);
    const removed=new Promise<void>(resolve=>a.socket.addEventListener("close",()=>resolve(),{once:true}));
    b.socket.send(JSON.stringify({type:"revoke",record:revokedRecord}));
    expect((await revocationNotice).type).toBe("revocations");
    await Promise.race([removed,Bun.sleep(5000).then(()=>{throw new Error("Revoked signaling socket stayed connected");})]);
    const c=await join();
    expect((await next(c.socket)).type).toBe("revocations");
    // Re-registration of the removed identity is refused even with its original valid proof.
    const denied=new WebSocket(`ws://127.0.0.1:45827/connect/${root.group_id}`);sockets.push(denied);
    const deniedChallenge=await next(denied);
    const deniedClosed=new Promise<void>(resolve=>denied.addEventListener("close",()=>resolve(),{once:true}));
    denied.send(JSON.stringify({type:"register",enrollment:a.enrollment,proof:await sign(a.key,"signal-register",{version:1,group_id:root.group_id,stage_id:a.device.stage_id,challenge:deniedChallenge.nonce,expires_at_ms:deniedChallenge.expires_at_ms})}));
    await Promise.race([deniedClosed,Bun.sleep(5000).then(()=>{throw new Error("Revoked device registered again");})]);
    const nameHash = await hash(encode.encode("DaatLocus.AccountLookup.v1/alice"));
    async function accountParticipant(publishing: boolean) {
      const key=await crypto.subtle.generateKey("Ed25519",true,["sign","verify"]) as CryptoKeyPair;
      const device={stage_id:crypto.randomUUID(),public_key:base64(await crypto.subtle.exportKey("raw",key.publicKey)),tls_sha256:"a".repeat(64)};
      const enrollment=publishing?{root,chain:[await sign(authority,"membership",{version:1,group_id:root.group_id,issuer:null,device,join_id:crypto.randomUUID(),issued_at_ms:Date.now(),expires_at_ms:Date.now()+300000,can_enroll:true})]}:null;
      const socket=new WebSocket(`ws://127.0.0.1:45827/accounts/${nameHash}`);sockets.push(socket);
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
  }
}, 30000);
