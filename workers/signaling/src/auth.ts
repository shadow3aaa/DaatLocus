// Matches src/stage/identity.rs: verify the exact signed payload bytes, not a
// provider-dependent reconstruction. The server's allowed roots are explicit.
export interface Proof { payload: string; signature: string }
export interface Device { stage_id: string; public_key: string; tls_sha256: string }
export interface Root { group_id: string; public_key: string }
export interface Enrollment { root: Root; chain: Proof[] }
interface Membership {
  version: number; group_id: string; issuer: string | null; device: Device;
  join_id: string; issued_at_ms: number; expires_at_ms: number; can_enroll: boolean;
}
export interface Registration {
  version: number; group_id: string; stage_id: string; challenge: string;
  expires_at_ms: number;
}
export interface Negotiation {
  version: number; group_id: string; source: string; target: string;
  generation: number; nonce: string; expires_at_ms: number;
  description: { type: "offer" | "answer"; sdp: string }; fingerprint: string;
}
const encode = new TextEncoder();
const decode = new TextDecoder("utf-8", { fatal: true, ignoreBOM: false });
const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
function assert(value: unknown): asserts value { if (!value) throw new Error("Unauthorized signaling"); }
export function bytes(value: string, max: number): Uint8Array<ArrayBuffer> {
  assert(typeof value === "string" && value.length <= Math.ceil(max / 3) * 4 && /^[A-Za-z0-9_-]*$/.test(value));
  const text = atob(value.replace(/-/g, "+").replace(/_/g, "/"));
  assert(text.length <= max);
  return Uint8Array.from(text, (character) => character.charCodeAt(0));
}
export async function hash(value: Uint8Array<ArrayBuffer>): Promise<string> {
  return Array.from(new Uint8Array(await crypto.subtle.digest("SHA-256", value)), (byte) => byte.toString(16).padStart(2, "0")).join("");
}
export async function verify<T>(proof: Proof, purpose: string, key: string): Promise<T> {
  const payload = bytes(proof.payload, 8192);
  const publicKey = bytes(key, 32); const signature = bytes(proof.signature, 64);
  assert(publicKey.length === 32 && signature.length === 64);
  const prefix = encode.encode(`DaatLocus.Stage.v1\0${purpose}\0`);
  const message = new Uint8Array(prefix.length + payload.length);
  message.set(prefix); message.set(payload, prefix.length);
  const imported = await crypto.subtle.importKey("raw", publicKey, "Ed25519", false, ["verify"]);
  assert(await crypto.subtle.verify("Ed25519", imported, signature, message));
  return JSON.parse(decode.decode(payload)) as T;
}
export async function membership(bundle: Enrollment, trusted: Root, revoked: Set<string>, now: number): Promise<Membership> {
  assert(bundle.root.group_id === trusted.group_id && bundle.root.public_key === trusted.public_key);
  assert(trusted.group_id === await hash(bytes(trusted.public_key, 32)));
  assert(Array.isArray(bundle.chain) && bundle.chain.length > 0 && bundle.chain.length <= 8);
  let key = trusted.public_key; let issuer: string | null = null;
  let expires = Number.MAX_SAFE_INTEGER; let canEnroll = true;
  let last: Membership | undefined; const seen = new Set<string>();
  for (const proof of bundle.chain) {
    const value = await verify<Membership>(proof, "membership", key);
    assert(value.version === 1 && value.group_id === trusted.group_id && value.issuer === issuer && canEnroll);
    assert(Number.isSafeInteger(value.issued_at_ms) && Number.isSafeInteger(value.expires_at_ms));
    assert(value.issued_at_ms <= now + 30000 && value.expires_at_ms > now && value.expires_at_ms <= expires);
    assert(value.device && uuid.test(value.device.stage_id) && uuid.test(value.join_id));
    assert(!seen.has(value.device.stage_id) && !revoked.has(value.device.stage_id));
    assert(/^[0-9a-f]{64}$/i.test(value.device.tls_sha256) && bytes(value.device.public_key, 32).length === 32);
    assert(value.device.public_key !== trusted.public_key && typeof value.can_enroll === "boolean");
    seen.add(value.device.stage_id); key = value.device.public_key; issuer = value.device.stage_id;
    expires = value.expires_at_ms; canEnroll = value.can_enroll; last = value;
  }
  assert(last); return last;
}
export async function registration(proof: Proof, member: Membership, group: string, challenge: string, now: number): Promise<void> {
  const value = await verify<Registration>(proof, "signal-register", member.device.public_key);
  assert(value.version === 1 && value.group_id === group && value.stage_id === member.device.stage_id && value.challenge === challenge);
  assert(Number.isSafeInteger(value.expires_at_ms) && value.expires_at_ms > now && value.expires_at_ms <= now + 30000);
}
export async function negotiation(proof: Proof, device: Device, group: string, now: number): Promise<Negotiation> {
  const value = await verify<Negotiation>(proof, "rtc-negotiation", device.public_key);
  assert(value.version === 1 && value.group_id === group && value.source === device.stage_id && uuid.test(value.target) && uuid.test(value.nonce));
  assert(Number.isSafeInteger(value.generation) && value.generation > 0 && Number.isSafeInteger(value.expires_at_ms) && value.expires_at_ms > now && value.expires_at_ms <= now + 120000);
  assert(value.description && ["offer", "answer"].includes(value.description.type) && typeof value.description.sdp === "string");
  assert(encode.encode(value.description.sdp).length <= 6144 && value.description.sdp.startsWith("v=0"));
  const fingerprints = value.description.sdp.split(/\r?\n/).filter((line) => line.startsWith("a=fingerprint:sha-256 ")).map((line) => line.slice(22).trim());
  assert(fingerprints.length > 0 && fingerprints.every((item) => item === value.fingerprint));
  assert(/^([0-9a-f]{2}:){31}[0-9a-f]{2}$/i.test(value.fingerprint));
  return value;
}
