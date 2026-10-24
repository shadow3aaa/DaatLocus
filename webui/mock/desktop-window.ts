import type { DesktopPlatform, WindowAction } from "../src/lib/desktop-window";

export type { DesktopPlatform, WindowAction };

let maximized = false;
export function desktopPlatform(): DesktopPlatform { return "macos"; }
export async function controlWindow(action: WindowAction) {
  if (action === "toggle_maximize") maximized = !maximized;
  return { maximized };
}
