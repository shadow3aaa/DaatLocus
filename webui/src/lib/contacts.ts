import { request, type ActorSummary, type FetchOptions, type Page } from "./actor-api";

export type ContactMessage = {message_id:string;kind:"input"|"reply"|"sent"|"assistant";content:string;result_id:string|null;generation:number|null};
export type ActorContact = {actor:ActorSummary;model:string|null;messages:ContactMessage[]};
export type ContactFocus = ContactMessage & {actorId:string};
export type ContactPage = Page<ActorContact,string> & {unavailable_devices:string[]};
export type ContactSort = "alphabetical" | "recent";
export function listContacts(query:string, options:FetchOptions & {after?:string;sort?:ContactSort}={}) {
  const params=new URLSearchParams({q:query,limit:"24",sort:options.sort??"alphabetical"});
  if(options.after)params.set("after",options.after);
  return request<ContactPage>(`/v1/contacts?${params}`,options);
}
export function contactExcerpt(content:string,query:string,radius=64) {
  const index=content.toLowerCase().indexOf(query.trim().toLowerCase());
  const start=Math.max(0,index-radius),end=Math.min(content.length,Math.max(0,index)+query.trim().length+radius);
  return `${start?"…":""}${content.slice(start,end)}${end<content.length?"…":""}`;
}
