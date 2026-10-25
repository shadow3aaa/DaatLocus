import { listen } from "@tauri-apps/api/event";

/** The shell relays WebView2 element-fullscreen state for its composed pages. */
export type BrowserFullscreen = { tab: string; fullscreen: boolean };

const TAB = /^[0-9a-f]{32}$/;

/** Parse a fullscreen notification; malformed payloads are ignored. */
export function browserFullscreenEvent(event: Event): BrowserFullscreen | null {
  const detail = (event as CustomEvent<unknown>).detail;
  if (!detail || typeof detail !== "object") return null;
  const { tab, fullscreen } = detail as { tab?: unknown; fullscreen?: unknown };
  return typeof tab === "string" && TAB.test(tab) && typeof fullscreen === "boolean"
    ? { tab, fullscreen }
    : null;
}

/** Only the visible page may take the shell's chrome away from it. */
export function browserFullscreenActive(fullscreen: string | null, selected: string | null): boolean {
  return fullscreen !== null && fullscreen === selected;
}

/** Subscribe to both the native event stream and the in-page fallback event. */
export async function subscribeBrowserFullscreen(callback: (value: BrowserFullscreen) => void) {
  const forward = (event: Event) => {
    const value = browserFullscreenEvent(event);
    if (value) callback(value);
  };
  window.addEventListener("dl-browser-fullscreen", forward);
  try {
    const stop = await listen<BrowserFullscreen>("dl-browser-fullscreen", event => {
      const value = event.payload;
      if (typeof value?.tab === "string" && TAB.test(value.tab) && typeof value?.fullscreen === "boolean") callback(value);
    });
    return () => {
      stop();
      window.removeEventListener("dl-browser-fullscreen", forward);
    };
  } catch (error) {
    window.removeEventListener("dl-browser-fullscreen", forward);
    throw error;
  }
}
