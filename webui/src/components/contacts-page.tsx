import { useCallback, useEffect, useId, useRef, useState } from "react";
import { ContactRoundIcon, CpuIcon, PanelLeftIcon, SearchIcon, ChevronRightIcon } from "lucide-react";
import { useTranslation } from "react-i18next";
import { ActorApiError, subscribeCatalogEvents } from "@/lib/actor-api";
import { actorIdFromUrl, localActorId } from "@/lib/conversation-links";
import { contactExcerpt, listContacts, type ActorContact, type ContactFocus, type ContactMessage } from "@/lib/contacts";
import { t } from "@/lib/i18n";
import { Button } from "@/components/ui/button";
import { GeneratedAvatar } from "@/components/generated-avatar";
import { actorAvatarSeed } from "@/lib/conversation-avatars";
import { Empty, EmptyHeader, EmptyMedia, EmptyTitle } from "@/components/ui/empty";
import { Skeleton } from "@/components/ui/skeleton";
import { FloatingSearchBar } from "@/components/floating-search";
import { HighlightedText } from "@/components/highlighted-text";
import { LazyPage } from "@/components/lazy-page";

export function ContactsPage({onNavigation,onActor,onAccessRevoked,active=true}:{onNavigation():void;onActor(id:string,focus?:ContactFocus):void;onAccessRevoked():void;active?:boolean}) {
  useTranslation();
  const [query,setQuery]=useState(""),[searching,setSearching]=useState(false);
  const [items,setItems]=useState<ActorContact[]>([]),[next,setNext]=useState<string|null>(null);
  const [loading,setLoading]=useState(true),[failed,setFailed]=useState(false),[unavailable,setUnavailable]=useState<string[]>([]);
  const [revision,setRevision]=useState(0),[cursor,setCursor]=useState<number|null>(null);
  const current=useRef<AbortController|null>(null),epoch=useRef(0),paging=useRef(false);
  const results=useId();
  const text=query.trim();
  const denied=useCallback((error:unknown)=>{if(error instanceof ActorApiError&&error.status===401)onAccessRevoked();},[onAccessRevoked]);
  useEffect(()=>{
    if(!active)return;
    const version=++epoch.current,controller=new AbortController();current.current=controller;
    setLoading(true);setFailed(false);setNext(null);setCursor(null);paging.current=false;
    const timer=setTimeout(()=>{
      void listContacts(text,{signal:controller.signal}).then(page=>{
        if(controller.signal.aborted||version!==epoch.current)return;
        setItems(page.items);setNext(page.next_cursor);setUnavailable(page.unavailable_devices);setCursor(page.event_cursor??0);
      }).catch(error=>{if(!controller.signal.aborted){denied(error);setFailed(true);}}).finally(()=>{if(!controller.signal.aborted)setLoading(false);});
    },text?180:0);
    return()=>{clearTimeout(timer);controller.abort();};
  },[text,revision,denied,active]);
  useEffect(()=>{
    if(cursor===null||!active)return;
    const controller=new AbortController();let timer:ReturnType<typeof setTimeout>|undefined;
    void subscribeCatalogEvents({after:cursor,signal:controller.signal,onEvent:record=>{
      if(record.event.type!=="actor_changed"&&record.event.type!=="actor_deleted"&&record.event.type!=="conversation_restored")return;
      clearTimeout(timer);timer=setTimeout(()=>setRevision(value=>value+1),300);
    }}).catch(error=>{if(!controller.signal.aborted)denied(error);});
    return()=>{controller.abort();clearTimeout(timer);};
  },[cursor,denied,active]);
  useEffect(()=>{
    if(!active)return;
    const key=(event:KeyboardEvent)=>{if(event.defaultPrevented||event.isComposing||event.altKey||event.shiftKey)return;if((event.ctrlKey||event.metaKey)&&["f","k"].includes(event.key.toLowerCase())){event.preventDefault();setSearching(true);}};
    window.addEventListener("keydown",key);return()=>window.removeEventListener("keydown",key);
  },[active]);
  async function more(signal:AbortSignal){
    if(!next||loading||paging.current||!current.current)return;
    const version=epoch.current;paging.current=true;
    try {
      const page=await listContacts(text,{after:next,signal:AbortSignal.any([signal,current.current.signal])});
      if(signal.aborted||version!==epoch.current)return;
      setItems(previous=>[...previous,...page.items.filter(item=>!previous.some(old=>old.actor.conversation_url===item.actor.conversation_url))]);
      setNext(page.next_cursor);setUnavailable(previous=>[...new Set([...previous,...page.unavailable_devices])]);
    } catch(error){if(!signal.aborted){denied(error);throw error;}}finally{if(version===epoch.current)paging.current=false;}
  }
  function open(item:ActorContact,message?:ContactMessage){
    const id=localActorId(actorIdFromUrl(item.actor.conversation_url,location.href)??item.actor.id);
    onActor(id,message?{...message,actorId:id}:undefined);
  }
  return <section className="dl-contacts-page" aria-label={t("contacts.title")} data-search-open={searching}>
    <header className="dl-apps-header" data-window-drag-region><div className="dl-apps-title"><Button variant="ghost" size="icon-sm" className="dl-apps-mobile-navigation" aria-label={t("navigation.open_sidebar")} onClick={onNavigation}><PanelLeftIcon/></Button><h1>{t("contacts.title")}</h1></div><Button variant="ghost" size="icon-sm" aria-label={t("contacts.search")} aria-keyshortcuts="Control+f Meta+f" onClick={()=>setSearching(true)}><SearchIcon/></Button></header>
    <FloatingSearchBar open={searching} query={query} onQueryChange={setQuery} onClose={()=>{setSearching(false);setQuery("");}} busy={loading} controls={results} label={t("contacts.search")} placeholder={t("contacts.search")}/>
    <div className="dl-contacts-scroll" id={results} aria-busy={loading}>
      {loading&&!items.length&&Array.from({length:6},(_,index)=><Skeleton key={index} className="mb-3 h-20 w-full rounded-xl"/>)}
      {!failed&&<ul className="dl-contact-list">{items.map(item=><li key={item.actor.conversation_url}>
        <button className="dl-contact-heading" onClick={()=>open(item)}><GeneratedAvatar seed={actorAvatarSeed(item.actor.id)} /><span><strong><HighlightedText text={item.actor.name} query={text}/></strong><small className="dl-contact-meta"><span>{t(`contacts.${item.actor.status}`)}</span><span className="dl-contact-model" aria-label={t("contacts.model",{model:item.model??t("contacts.no_model")})}><CpuIcon aria-hidden="true"/>{item.model??t("contacts.no_model")}</span></small></span><ChevronRightIcon aria-hidden="true"/></button>
        {item.messages.length>0&&<div className="dl-contact-matches">{item.messages.map(message=><button key={`${message.message_id}:${message.kind}:${message.result_id??message.generation??""}`} onClick={()=>open(item,message)}><small>{t(`contacts.${message.kind}`)}</small><span><HighlightedText text={contactExcerpt(message.content,text)} query={text}/></span></button>)}</div>}
      </li>)}</ul>}
      {!loading&&!failed&&!items.length&&!next&&<Empty><EmptyHeader><EmptyMedia variant="icon"><ContactRoundIcon/></EmptyMedia><EmptyTitle>{t(text?"contacts.no_results":"contacts.empty")}</EmptyTitle></EmptyHeader></Empty>}
      {failed&&<div className="dl-client-error" role="alert"><span>{t("lazy_page.load_failed")}</span><Button variant="ghost" size="sm" onClick={()=>setRevision(value=>value+1)}>{t("app.retry")}</Button></div>}
      {next&&<LazyPage key={text} cursor={next} onLoad={more} errorMessage={t("lazy_page.load_failed")}/>}
      {unavailable.length>0&&<div className="dl-client-error" role="status"><span>{t("contacts.unavailable",{count:unavailable.length})}</span><Button variant="ghost" size="sm" onClick={()=>setRevision(value=>value+1)}>{t("app.retry")}</Button></div>}
    </div>
  </section>;
}
