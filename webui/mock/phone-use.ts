/** Explicit development preview only; production always uses the native connection. */
import type { PhoneUseDisplay } from "../src/lib/phone-use-display";
import { validPhoneUseDisplay } from "../src/lib/phone-use-display";
type Method = "shizuku" | "root";
type Status = {enabled: boolean; method: Method; state: "disabled" | "ready"; uid: number | null; display: PhoneUseDisplay};
let status: Status = {enabled: false, method: "shizuku", state: "disabled", uid: null, display: {width: 1280, height: 720, density: 160}};
export const getPhoneUseStatus = async () => structuredClone(status);
export async function configurePhoneUse(enabled: boolean, method: Method) {
  await new Promise(resolve => setTimeout(resolve, 500));
  status = {...status, enabled, method, state: enabled ? "ready" : "disabled", uid: enabled ? method === "root" ? 0 : 2000 : null};
  return structuredClone(status);
}
export async function configurePhoneUseDisplay(patch: Partial<PhoneUseDisplay>) {
  const display = {...status.display, ...patch};
  if (!validPhoneUseDisplay(display)) throw new Error("Invalid display settings");
  status = {...status, display};
  return structuredClone(status);
}
