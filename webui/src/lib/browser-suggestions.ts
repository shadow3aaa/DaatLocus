import {browserDestination} from "./browser-address";
import type {BrowserPageInfo,BrowserSuggestions} from "./browser-workspace";

export type SuggestionFilter="history"|"bookmarks"|"tabs"|null;
export type AddressSuggestion={key:string;kind:"search"|"search_history"|"history"|"bookmark"|"tab";label:string;value:string;detail?:string;favicon?:string|null;id?:string;tab?:string};
export function addressSuggestions(query:string,filter:SuggestionFilter,data:BrowserSuggestions,pages:BrowserPageInfo[],engine:string):AddressSuggestion[]{
  const text=query.trim(),words=text.toLocaleLowerCase().split(/\s+/).filter(Boolean);
  const matches=(value:string)=>words.every(word=>value.toLocaleLowerCase().includes(word));
  const items:AddressSuggestion[]=[];
  const seen=new Set<string>();
  const add=(item:AddressSuggestion)=>{const key=item.kind==="tab"?`tab:${item.tab}`:item.value;if(!seen.has(key)){seen.add(key);items.push(item);}};
  if(text&&!filter&&browserDestination(text,engine)){
    const value=browserDestination(text,engine)!;
    add({key:"input",kind:"search",label:text,value:text,detail:value.startsWith(engine)?"search":"navigate"});
  }
  const addTabs=()=>{for(const page of pages)if(page.url!=="about:blank"&&matches(`${page.title} ${page.url}`))add({key:`tab:${page.tab}`,kind:"tab",tab:page.tab,label:page.title||page.url,value:page.url,detail:page.url,favicon:page.favicon});};
  if(filter==="tabs"||text&&!filter)addTabs();
  if(!filter||filter==="history")for(const search of data.searches)if(matches(search.query))add({key:`search:${search.id}`,kind:"search_history",id:search.id,label:search.query,value:search.query});
  if(!filter||filter==="bookmarks")for(const item of data.bookmarks)if(matches(`${item.title} ${item.url}`))add({key:`bookmark:${item.id}`,kind:"bookmark",id:item.id,label:item.title||item.url,value:item.url,detail:item.url,favicon:item.favicon});
  if(!filter||filter==="history")for(const item of data.history)if(matches(`${item.title} ${item.url}`))add({key:`history:${item.id}`,kind:"history",id:item.id,label:item.title||item.url,value:item.url,detail:item.url,favicon:item.favicon});
  if(!text&&!filter)addTabs();
  return items.slice(0,12);
}
