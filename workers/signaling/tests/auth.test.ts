import { describe, expect, test } from "bun:test";
import { hash, membership, registration, type Proof, type Root, type Device } from "../src/auth";
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
describe("relay authentication", () => {
  test("independent device registration binds purpose, challenge and expiry", async () => {
    const fixture = await setup(); const member = await membership(fixture.enrollment, fixture.root, new Set(), 2000);
    const challenge = crypto.randomUUID(); const value = { version: 2, group_id: fixture.root.group_id, stage_id: fixture.device.stage_id, challenge, expires_at_ms: 10000 };
    const proof = await sign(fixture.deviceKey, "relay-register", value);
    await registration(proof, member, fixture.root.group_id, challenge, 2000,10000);
    await expect(registration(proof, member, fixture.root.group_id, crypto.randomUUID(), 2000,10000)).rejects.toThrow();
    await expect(registration(await sign(fixture.deviceKey, "membership", value), member, fixture.root.group_id, challenge, 2000,10000)).rejects.toThrow();
    await expect(registration(proof, member, fixture.root.group_id, challenge, 10000,10000)).rejects.toThrow();
  });
  test("revoked members and forged group roots are rejected", async () => {
    const fixture = await setup();
    await expect(membership(fixture.enrollment, fixture.root, new Set([fixture.device.stage_id]), 2000)).rejects.toThrow();
    const other = await setup(); await expect(membership(fixture.enrollment, other.root, new Set(), 2000)).rejects.toThrow();
    await expect(membership(fixture.enrollment, fixture.root, new Set(), 100000)).rejects.toThrow();
  });
  test("small device clock offsets do not reject freshly issued membership", async () => {
    const fixture = await setup();
    const issued = 100000;
    const proof = await sign(fixture.authority, "membership", {
      version: 1, group_id: fixture.root.group_id, issuer: null, device: fixture.device,
      join_id: crypto.randomUUID(), issued_at_ms: issued, expires_at_ms: issued + 60000, can_enroll: true,
    });
    const enrollment = { root: fixture.root, chain: [proof] };
    for (const now of [issued - 30000, issued - 1000, issued, issued + 1000]) {
      expect((await membership(enrollment, fixture.root, new Set(), now)).device.stage_id).toBe(fixture.device.stage_id);
    }
    for (const now of [issued - 30001, issued + 60000]) {
      await expect(membership(enrollment, fixture.root, new Set(), now)).rejects.toThrow();
    }
    await expect(membership(enrollment, fixture.root, new Set([fixture.device.stage_id]), issued - 1000)).rejects.toThrow();
  });
});
