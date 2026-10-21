import { ActorApiError, request, type FetchOptions } from "./actor-api";
import { serviceFetch } from "./transport";

export type VirtualDisplay = { id: string; actor_id: string | null; name: string | null; width: number; height: number };
export type DisplayCatalog = { supported: boolean; state: "ready" | "disabled" | "connecting" | "unavailable" | "unsupported"; displays: VirtualDisplay[] };
export type DisplayPreview = { catalog: DisplayCatalog | null; display: VirtualDisplay | null; image: Blob | null; failed: boolean };
const path = "/v1/device/virtual-displays";
export const virtualDisplayCapabilities = (options?: FetchOptions) => request<{ supported: boolean }>(`${path}/capabilities`, options);
export const virtualDisplayCatalog = (signal: AbortSignal) => request<DisplayCatalog>(path, { signal });
export async function virtualDisplayFrame(id: string, signal: AbortSignal): Promise<Blob | null> {
  const response = await serviceFetch(`${path}/${encodeURIComponent(id)}/frame`, { signal, credentials: "same-origin", cache: "no-store", headers: { Accept: "image/png" } });
  if (!response.ok) throw new ActorApiError("Virtual display unavailable", response.status);
  if (response.status === 204) return null;
  if (response.headers.get("content-type")?.split(";")[0] !== "image/png") throw new Error("Invalid display image");
  return response.blob();
}

function pause(signal: AbortSignal): Promise<void> {
  return new Promise(resolve => {
    const done = () => { clearTimeout(timer); signal.removeEventListener("abort", done); resolve(); };
    const timer = setTimeout(done, 1000);
    signal.addEventListener("abort", done, { once: true });
    if (signal.aborted) done();
  });
}

/** Sequential pull: never queue captures; abort fences every publication. */
export async function observeVirtualDisplays(
  signal: AbortSignal,
  selected: string | null,
  publish: (preview: DisplayPreview) => void,
  revoked: () => void,
  api = { catalog: virtualDisplayCatalog, frame: virtualDisplayFrame, pause },
): Promise<void> {
  while (!signal.aborted) {
    let catalog: DisplayCatalog | null = null;
    let display: VirtualDisplay | null = null;
    try {
      catalog = await api.catalog(signal);
      if (signal.aborted) return;
      display = catalog.state === "ready" ? catalog.displays.find(item => item.id === selected) ?? catalog.displays[0] ?? null : null;
      const image = display ? await api.frame(display.id, signal) : null;
      if (signal.aborted) return;
      publish({ catalog, display, image, failed: false });
    } catch (error) {
      if (signal.aborted) return;
      publish({ catalog, display, image: null, failed: true });
      if (error instanceof ActorApiError && [401, 403, 410].includes(error.status)) { revoked(); return; }
    }
    await api.pause(signal);
  }
}
