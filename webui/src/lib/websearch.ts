import { request, type FetchOptions } from "./actor-api";

export type SearchProvider = "opencode" | "tavily" | "exa" | "brave" | "serper" | "firecrawl" | "parallel";
export const searchProviderNames: Record<SearchProvider, string> = {
  opencode: "OpenCode", tavily: "Tavily", exa: "Exa", brave: "Brave", serper: "Google · Serper", firecrawl: "Firecrawl", parallel: "Parallel",
};
export type SearchSettings = {
  provider: SearchProvider | null;
  providers: { id: SearchProvider; credential_configured: boolean }[];
};
export const getSearchSettings = (options?: FetchOptions) => request<SearchSettings>("/v1/config/websearch", options);
export const patchSearchSettings = (patch: { provider: SearchProvider; api_key?: string }) => request<SearchSettings>("/v1/config/websearch", { method: "PATCH", body: patch });

type Patch = {provider: SearchProvider; api_key?: string};
type Snapshot = {settings: SearchSettings | null; key: string; pending: boolean; error: string | null};
/** Kept by SettingsPage so failed edits survive category and dialog navigation. */
export class WebSearchPreferences {
  private snapshot: Snapshot = {settings:null,key:"",pending:false,error:null};
  private listeners = new Set<() => void>();
  private saves = Promise.resolve();
  private revision = 0;
  private retained: Patch | null = null;
  constructor(private api = {get:getSearchSettings, patch:patchSearchSettings}) {}
  getSnapshot = () => this.snapshot;
  subscribe = (listener: () => void) => {this.listeners.add(listener); return () => {this.listeners.delete(listener);};};
  private publish(patch: Partial<Snapshot>) {this.snapshot={...this.snapshot,...patch}; this.listeners.forEach(listener=>listener());}
  async load(signal?: AbortSignal) {
    if (this.snapshot.settings) return;
    try {const settings=await this.api.get({signal}); if (!signal?.aborted) this.publish({settings,error:null});}
    catch (cause) {if (!signal?.aborted) this.publish({error:String(cause)});}
  }
  editKey(key: string) {this.publish({key});}
  save(patch: Patch): Promise<void> {
    const revision=++this.revision;
    this.publish({pending:true,error:null,settings:this.snapshot.settings?{...this.snapshot.settings,provider:patch.provider}:null});
    this.saves=this.saves.then(async()=>{
      try {
        const settings=await this.api.patch(patch);
        this.retained=null;
        if(revision===this.revision)this.publish({settings,pending:false,error:null,...(this.snapshot.key===patch.api_key?{key:""}:{})});
      } catch(cause) {
        this.retained=patch;
        if(revision===this.revision)this.publish({pending:false,error:String(cause)});
      }
    });
    return this.saves;
  }
  retry() {return this.retained?this.save(this.retained):this.load();}
}
