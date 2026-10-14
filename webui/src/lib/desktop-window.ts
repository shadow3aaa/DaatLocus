import { invoke, isTauri } from "@tauri-apps/api/core";

export type DesktopPlatform = "windows" | "macos";
export function desktopPlatform(): DesktopPlatform | null {
  if (typeof window === "undefined" || !isTauri()) return null;
  const platform = (window as Window & { __DAAT_LOCUS_DESKTOP__?: string }).__DAAT_LOCUS_DESKTOP__;
  return platform === "windows" || platform === "macos" ? platform : null;
}
export type WindowAction = "state" | "drag" | "minimize" | "toggle_maximize" | "close";
export const controlWindow = (action: WindowAction) => invoke<{ maximized: boolean }>("desktop_window", { action });
