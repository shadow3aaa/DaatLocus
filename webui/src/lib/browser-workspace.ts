import { invoke, isTauri } from "@tauri-apps/api/core";

export type BrowserPageInfo = { tab: string; url: string; title: string; loading?: boolean; can_back?: boolean; can_forward?: boolean; zoom?: number };
export type BrowserWorkspace = { pages: BrowserPageInfo[]; selected: string | null };
export type BrowserVisit = { id: string; url: string; title: string; time: number };
export type BrowserDownload = { id: string; url: string; name: string; time: number; status: "downloading" | "complete" | "failed" | "interrupted"; bytes: number };
export type BrowserLibrary = { history: BrowserVisit[]; downloads: BrowserDownload[]; next_offset?: number | null };
export type BrowserBounds = { x: number; y: number; width: number; height: number; scale: number };
export type BrowserOcclusion = { x:number; y:number; width:number; height:number; radius:number };
export type BrowserFindResult = { found: boolean; count: number | null; index: number | null };
export type BrowserFindAction = "find" | "find_next" | "find_previous" | "find_close";
export type BrowserAction = "list" | "select" | "move_tab" | "open" | "new_tab" | "navigate" | "back" | "forward" | "reload" | "stop" | "close";
export type BrowserLibraryAction = "library" | "clear_history" | "remove_history" | "save_download" | "remove_download" | "reveal_download" | "open_downloads" | "zoom" | "print" | "clear_data";
export const browserSupported = isTauri;
let commands: Promise<unknown> = Promise.resolve();
let layouts: Promise<unknown> = Promise.resolve();
export function browserControl(action: BrowserAction, tab: string | null = null, url: string | null = null) {
  const result = commands.catch(() => {}).then(() => invoke<BrowserWorkspace>("browser_control", { action, tab, url, bounds: null }));
  commands = result;
  return result;
}
export function browserLibrary(action: BrowserLibraryAction, value: string | null = null) {
  return invoke<BrowserLibrary>("browser_control", {action,tab:null,url:value,bounds:null});
}
let searches: Promise<unknown> = Promise.resolve();
export function browserFind(action: BrowserFindAction, text: string, tab: string) {
  const result = searches.catch(() => {}).then(() => invoke<BrowserFindResult>("browser_control", { action, tab, url: text, bounds: null }));
  searches = result;
  return result;
}
export function browserScreenshot(tab: string) {
  return invoke<{ destination: "clipboard" | "download" | "cancelled" }>("browser_control", { action: "screenshot", tab, url: null, bounds: null });
}
// Route changes cannot wait behind a slow navigation. Layout updates retain
// their own order, including cleanup from the previous mounted page.
export function browserLayout(bounds: BrowserBounds | null, occlusions:BrowserOcclusion[] = []) {
  const result = layouts.catch(() => {}).then(() => invoke("browser_control", { action: bounds ? "mount" : "detach", bounds, occlusions, tab: null, url: null }));
  layouts = result;
  return result;
}
