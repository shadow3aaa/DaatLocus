import { isTauri } from "@tauri-apps/api/core";
export function isAndroid(): boolean {
  return typeof window !== "undefined" && isTauri() && (window as Window & { __DAAT_LOCUS_MOBILE__?: string }).__DAAT_LOCUS_MOBILE__ === "android";
}
