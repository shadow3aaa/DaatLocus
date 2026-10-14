import { describe, expect, test } from "bun:test";
import { hash, membership, registration, negotiation, type Proof, type Root, type Device } from "../src/auth";
const encoder = new TextEncoder();
function b64(bytes: ArrayBuffer | Uint8Array): string { return Buffer.from(bytes).toString("base64url"); }
async function key() { return await crypto.subtle.generateKey("Ed25519", true, ["sign", "verify"]) as CryptoKeyPair; }
async function sign(pair: CryptoKeyPair, purpose: string, value: unknown): Promise<Proof> {
  const payload = encoder.encode(JSON.stringify(value)); const prefix = encoder.encode(`DaatLocus.Stage.v1\0${purpose}\0`);
  const message = new Uint8Array(prefix.length + payload.length); message.set(prefix); message.set(payload, prefix.length);
  return { payload: b64(payload), signature: b64(await crypto.subtle.sign("Ed25519", pair.privateKey, message)) };
}
async function setup() {
  const authority = await key(); const deviceKey = await key();
  const publicAuthority = new Uint8Array(await crypto.subtle.exportKey("raw", authority.publicKey));
  const root: Root = { group_id: await hash(publicAuthority), public_key: b64(publicAuthority) };
  const device: Device = { stage_id: crypto.randomUUID(), public_key: b64(await crypto.subtle.exportKey("raw", deviceKey.publicKey)), tls_sha256: "a".repeat(64) };
  const proof = await sign(authority, "membership", { version: 1, group_id: root.group_id, issuer: null, device, join_id: crypto.randomUUID(), issued_at_ms: 1000, expires_at_ms: 100000, can_enroll: true });
  return { authority, deviceKey, root, device, enrollment: { root, chain: [proof] } };
}
describe("signaling authentication", () => {
  test("independent device registration binds purpose, challenge and expiry", async () => {
    const fixture = await setup(); const member = await membership(fixture.enrollment, fixture.root, new Set(), 2000);
    const challenge = crypto.randomUUID(); const value = { version: 1, group_id: fixture.root.group_id, stage_id: fixture.device.stage_id, challenge, expires_at_ms: 10000 };
    const proof = await sign(fixture.deviceKey, "signal-register", value);
    await registration(proof, member, fixture.root.group_id, challenge, 2000);
    expect(registration(proof, member, fixture.root.group_id, crypto.randomUUID(), 2000)).rejects.toThrow();
    expect(registration(await sign(fixture.deviceKey, "membership", value), member, fixture.root.group_id, challenge, 2000)).rejects.toThrow();
    expect(registration(proof, member, fixture.root.group_id, challenge, 10000)).rejects.toThrow();
  });
  test("revoked members and forged group roots are rejected", async () => {
    const fixture = await setup();
    expect(membership(fixture.enrollment, fixture.root, new Set([fixture.device.stage_id]), 2000)).rejects.toThrow();
    const other = await setup(); expect(membership(fixture.enrollment, other.root, new Set(), 2000)).rejects.toThrow();
    expect(membership(fixture.enrollment, fixture.root, new Set(), 100000)).rejects.toThrow();
  });
  test("only signed SDP with the same DTLS fingerprint is forwarded", async () => {
    const fixture = await setup(); const fingerprint = Array(32).fill("ab").join(":");
    const value = { version: 1, group_id: fixture.root.group_id, source: fixture.device.stage_id, target: crypto.randomUUID(), generation: 1, nonce: crypto.randomUUID(), expires_at_ms: 10000, description: { type: "offer", sdp: `v=0\r\na=fingerprint:sha-256 ${fingerprint}\r\n` }, fingerprint };
    const proof = await sign(fixture.deviceKey, "rtc-negotiation", value);
    expect((await negotiation(proof, fixture.device, fixture.root.group_id, 2000)).nonce).toBe(value.nonce);
    expect(negotiation(await sign(fixture.deviceKey, "rtc-negotiation", { ...value, fingerprint: "cd".repeat(32) }), fixture.device, fixture.root.group_id, 2000)).rejects.toThrow();
  });
});
