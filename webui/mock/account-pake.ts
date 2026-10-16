/** Resolved only by the development preview plugin. It cannot authenticate or
 * generate credentials; normal and distribution builds use the real WASM. */
export default async function initialize(): Promise<never> {throw new Error("Authentication is unavailable in the visual preview");}
export class Login {
  constructor(_password: Uint8Array) {}
  request(): Uint8Array {throw new Error("Visual preview");}
  finish(_response: Uint8Array, _context?: Uint8Array): Uint8Array {throw new Error("Visual preview");}
  open(_record: Uint8Array): Uint8Array {throw new Error("Visual preview");}
  free(): void {}
}
export class Registration extends Login {}
