import { useEffect, useSyncExternalStore } from "react";
import { browserPreferences, type BrowserPreferences } from "./browser-workspace";

export const searchEngines = {
  google: "https://www.google.com/search?q=",
  bing: "https://www.bing.com/search?q=",
  duckduckgo: "https://duckduckgo.com/?q=",
  baidu: "https://www.baidu.com/s?wd=",
};
let snapshot:{value:BrowserPreferences;loading:boolean;pending:boolean;error:string|null}={value:{search_engine:"google",language:"system",default_zoom:1},loading:true,pending:false,error:null};
const listeners=new Set<()=>void>();
let started=false;
let failed:Partial<BrowserPreferences>|undefined;
const notify=()=>listeners.forEach(listener=>listener());
async function load(){
  try{
    let value=await browserPreferences();
    const legacy=localStorage.getItem("dl.browser.search");
    if(legacy){const engine=Object.entries(searchEngines).find(([,url])=>url===legacy)?.[0] as BrowserPreferences["search_engine"]|undefined;if(engine)value=await browserPreferences({search_engine:engine});localStorage.removeItem("dl.browser.search");}
    snapshot={value,loading:false,pending:false,error:null};
  }catch(error){snapshot={...snapshot,loading:false,error:String(error)};}
  notify();
}
async function update(patch:Partial<BrowserPreferences>){
  if(snapshot.pending||snapshot.loading)return;
  patch={...failed,...patch};
  snapshot={...snapshot,value:{...snapshot.value,...patch},pending:true,error:null};notify();
  try{const value=await browserPreferences(patch);failed=undefined;snapshot={...snapshot,value,pending:false};}
  catch(error){failed=patch;snapshot={...snapshot,pending:false,error:String(error)};}
  notify();
}
export function useBrowserPreferences(){
  const state=useSyncExternalStore(callback=>{listeners.add(callback);return()=>{listeners.delete(callback);};},()=>snapshot);
  useEffect(()=>{if(!started){started=true;void load();}},[]);
  return {...state,update,retry:()=>failed?update(failed):load()};
}
