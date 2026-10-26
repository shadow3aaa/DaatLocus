import { latestOperation } from "./latest-operation";
import { listen } from "@tauri-apps/api/event";
import { invoke, isTauri } from "@tauri-apps/api/core";

export type BrowserPageInfo = { tab: string; url: string; title: string; favicon?: string | null; background?: string | null; progress?: number; loading?: boolean; can_back?: boolean; can_forward?: boolean; zoom?: number };
export type BrowserWorkspace = { pages: BrowserPageInfo[]; selected: string | null };
export type BrowserVisit = { id: string; url: string; title: string; time: number; favicon?: string | null };
export type BrowserBookmark = BrowserVisit;
export type BrowserSearch = {id:string;query:string;time:number};
export type BrowserSuggestions = {history:BrowserVisit[];bookmarks:BrowserBookmark[];searches:BrowserSearch[]};
export type BookmarkPage = {items:BrowserBookmark[];next_offset:number|null};
export type SavedAction = "suggestions"|"bookmarks"|"bookmark"|"save_bookmark"|"remove_bookmark"|"record_search"|"remove_search";
export async function browserSaved<T>(action:SavedAction,input:object={}){
  const value=await invoke<T>("browser_control",{action,url:JSON.stringify(input),tab:null,bounds:null});
  if(!["suggestions","bookmarks","bookmark"].includes(action))window.dispatchEvent(new Event("dl-browser-saved-changed"));
  return value;
}
export type BrowserPreferences = {search_engine:"google"|"bing"|"duckduckgo"|"baidu";language:"system"|"zh-CN"|"en-US";default_zoom:number};
export function browserPreferences(patch?:Partial<BrowserPreferences>){return invoke<BrowserPreferences>("browser_control",{action:patch?"set_preferences":"preferences",url:patch?JSON.stringify(patch):null,tab:null,bounds:null});}
export type BrowserDownload = { id: string; url: string; name: string; time: number; status: "downloading" | "complete" | "failed" | "interrupted"; bytes: number };
export type BrowserLibrary = { history: BrowserVisit[]; downloads: BrowserDownload[]; next_offset?: number | null };
export type BrowserBounds = { x: number; y: number; width: number; height: number; scale: number };
export type BrowserHitRegion = { x:number; y:number; width:number; height:number; radius:number };
export type BrowserFindResult = { found: boolean; count: number | null; index: number | null };
export type BrowserFindAction = "find" | "find_next" | "find_previous" | "find_close";
export type BrowserAction = "list" | "select" | "move_tab" | "open" | "new_tab" | "navigate" | "back" | "forward" | "reload" | "stop" | "close";
export type BrowserLibraryAction = "library" | "clear_history" | "remove_history" | "save_download" | "remove_download" | "reveal_download" | "open_downloads" | "zoom" | "print" | "clear_data";
export const browserSupported = isTauri;
let commands: Promise<unknown> = Promise.resolve();

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
const applyLayout=latestOperation<{bounds:BrowserBounds|null;foreground:BrowserHitRegion[]}>(value=>invoke("browser_control", {action:value.bounds?"mount":"detach",...value,tab:null,url:null}));
export function browserLayout(bounds:BrowserBounds|null,foreground:BrowserHitRegion[]=[]){return applyLayout({bounds,foreground});}
export async function subscribeBrowserWorkspace(callback:()=>void){
 window.addEventListener("dl-browser-changed",callback);
 try{const stop=await listen("dl-browser-changed",callback);return()=>{stop();window.removeEventListener("dl-browser-changed",callback);};}
 catch(error){window.removeEventListener("dl-browser-changed",callback);throw error;}
}
