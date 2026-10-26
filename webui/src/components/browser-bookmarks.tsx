import {useEffect,useRef,useState} from "react";
import {PencilIcon,PlusIcon,SearchIcon,StarIcon,Trash2Icon} from "lucide-react";
import {t} from "@/lib/i18n";
import {browserSaved,type BrowserBookmark,type BrowserPageInfo,type BookmarkPage} from "@/lib/browser-workspace";
import {browserDestination} from "@/lib/browser-address";
import {Button} from "@/components/ui/button";
import {Input} from "@/components/ui/input";
import {Field,FieldGroup,FieldLabel} from "@/components/ui/field";
import {Dialog,DialogContent,DialogTitle,DialogFooter} from "@/components/ui/dialog";
import {Alert,AlertDescription} from "@/components/ui/alert";
import {Empty,EmptyHeader,EmptyMedia,EmptyTitle} from "@/components/ui/empty";
import {InputGroup,InputGroupAddon,InputGroupInput} from "@/components/ui/input-group";
import {BrowserSiteIcon} from "./browser-site-icon";

type Draft=Partial<BrowserBookmark>;
function BookmarkEditor({draft,open,onClose}:{draft:Draft;open:boolean;onClose():void}){
  const [title,setTitle]=useState(draft.title??""),[url,setUrl]=useState(draft.url??""),[pending,setPending]=useState(false),[failed,setFailed]=useState(false),[invalid,setInvalid]=useState(false);
  useEffect(()=>{if(open){setTitle(draft.title??"");setUrl(draft.url??"");setFailed(false);setInvalid(false);}},[open,draft]);
  async function save(){
    const destination=browserDestination(url,"");
    if(!destination||!/^https?:\/\//.test(destination)){setInvalid(true);return;}
    setPending(true);setFailed(false);
    const favicon=draft.url&&new URL(draft.url).origin===new URL(destination).origin?draft.favicon??null:null;
    try{await browserSaved("save_bookmark",{id:draft.id??null,title,url:destination,favicon});onClose();}catch{setFailed(true);}finally{setPending(false);}
  }
  async function remove(){setPending(true);setFailed(false);try{await browserSaved("remove_bookmark",{id:draft.id});onClose();}catch{setFailed(true);}finally{setPending(false);}}
  return <Dialog open={open} onOpenChange={value=>{if(!value&&!pending)onClose();}}><DialogContent className="dl-browser-bookmark-editor" aria-describedby={undefined} showCloseButton={!pending}>
    <DialogTitle>{t(draft.id?"browser_use.edit_bookmark":draft.url?"browser_use.add_bookmark":"browser_use.new_bookmark")}</DialogTitle>
    <form onSubmit={event=>{event.preventDefault();event.stopPropagation();if(!pending)void save();}}>
      <FieldGroup><Field><FieldLabel htmlFor="dl-bookmark-title">{t("browser_use.bookmark_name")}</FieldLabel><Input id="dl-bookmark-title" value={title} onChange={event=>setTitle(event.target.value)} disabled={pending} autoComplete="off"/></Field>
      <Field data-invalid={invalid||undefined}><FieldLabel htmlFor="dl-bookmark-url">{t("browser_use.address")}</FieldLabel><Input id="dl-bookmark-url" aria-invalid={invalid} value={url} onChange={event=>{setUrl(event.target.value);setInvalid(false);}} disabled={pending} spellCheck={false} autoCapitalize="off" autoComplete="url"/>{invalid&&<span role="alert">{t("browser_use.invalid_bookmark_url")}</span>}</Field></FieldGroup>
      {failed&&<Alert variant="destructive"><AlertDescription>{t("browser_use.bookmarks_failed")}</AlertDescription></Alert>}
      <DialogFooter>{draft.id&&<Button type="button" variant="ghost" disabled={pending} onClick={()=>void remove()}><Trash2Icon data-icon="inline-start"/>{t("browser_use.remove_bookmark")}</Button>}<Button type="button" variant="outline" disabled={pending} onClick={onClose}>{t("common.cancel")}</Button><Button type="submit" disabled={pending}>{t("app.save")}</Button></DialogFooter>
    </form>
  </DialogContent></Dialog>;
}
export function BrowserBookmarkButton({page}:{page?:BrowserPageInfo}){
  const [bookmark,setBookmark]=useState<BrowserBookmark|null>(null),[editing,setEditing]=useState<Draft|null>(null),[failed,setFailed]=useState(false);
  const url=page?.url;
  useEffect(()=>{
    let live=true;setBookmark(null);
    const load=()=>{if(!url||url==="about:blank")return;void browserSaved<{item:BrowserBookmark|null}>("bookmark",{url}).then(value=>{if(live){setBookmark(value.item);setFailed(false);}}).catch(()=>{if(live)setFailed(true);});};
    load();window.addEventListener("dl-browser-saved-changed",load);return()=>{live=false;window.removeEventListener("dl-browser-saved-changed",load);};
  },[url]);
  useEffect(()=>{function edit(){if(page&&url!=="about:blank")setEditing(bookmark??{title:page.title,url:page.url,favicon:page.favicon});}window.addEventListener("dl-browser-bookmark-current",edit);return()=>window.removeEventListener("dl-browser-bookmark-current",edit);},[page,bookmark,url]);
  return <><Button type="button" variant="ghost" size="icon-sm" disabled={!url||url==="about:blank"} aria-label={t(bookmark?"browser_use.edit_bookmark":"browser_use.add_bookmark")} aria-pressed={Boolean(bookmark)} title={t(failed?"browser_use.bookmarks_failed":bookmark?"browser_use.edit_bookmark":"browser_use.add_bookmark")} onClick={()=>setEditing(bookmark??{title:page?.title,url,favicon:page?.favicon})}><StarIcon fill={bookmark?"currentColor":"none"}/></Button>{editing&&<BookmarkEditor draft={editing} open onClose={()=>setEditing(null)}/>}</>;
}
export function BrowserBookmarks({onNavigate}:{onNavigate(url:string):void}){
  const [query,setQuery]=useState(""),[page,setPage]=useState<BookmarkPage>({items:[],next_offset:null}),[failed,setFailed]=useState(false),[revision,setRevision]=useState(0),[editing,setEditing]=useState<Draft|null>(null);
  const request=useRef(0);
  const [pending,setPending]=useState(false);
  useEffect(()=>{const update=()=>setRevision(value=>value+1);window.addEventListener("dl-browser-saved-changed",update);return()=>window.removeEventListener("dl-browser-saved-changed",update);},[]);
  useEffect(()=>{const id=++request.current;setPending(true);const timer=setTimeout(()=>{void browserSaved<BookmarkPage>("bookmarks",{query,offset:0}).then(value=>{if(request.current===id){setPage(value);setFailed(false);}}).catch(()=>{if(request.current===id)setFailed(true);}).finally(()=>{if(request.current===id)setPending(false);});},query?100:0);return()=>{++request.current;clearTimeout(timer);};},[query,revision]);
  async function more(){if(pending||page.next_offset===null)return;const id=++request.current;setPending(true);try{const value=await browserSaved<BookmarkPage>("bookmarks",{query,offset:page.next_offset});if(request.current===id)setPage(previous=>({items:[...previous.items,...value.items],next_offset:value.next_offset}));}catch{if(request.current===id)setFailed(true);}finally{if(request.current===id)setPending(false);}}
  return <section className="dl-browser-bookmarks"><div className="dl-browser-bookmarks-tools"><InputGroup><InputGroupAddon><SearchIcon/></InputGroupAddon><InputGroupInput aria-label={t("browser_use.search_bookmarks")} placeholder={t("browser_use.search_bookmarks")} value={query} onChange={event=>setQuery(event.target.value)}/></InputGroup><Button variant="outline" onClick={()=>setEditing({})}><PlusIcon data-icon="inline-start"/>{t("browser_use.new_bookmark")}</Button></div>
    {failed&&<Alert variant="destructive"><AlertDescription>{t("browser_use.bookmarks_failed")}</AlertDescription></Alert>}
    <div className="dl-browser-recent">{page.items.map(item=><div className="dl-browser-history-row" key={item.id}><button type="button" onClick={()=>onNavigate(item.url)}><span className="dl-browser-row-icon"><BrowserSiteIcon src={item.favicon}/></span><span><strong>{item.title}</strong><small>{item.url}</small></span></button><Button variant="ghost" size="icon-sm" aria-label={t("browser_use.edit_bookmark_named",{title:item.title})} onClick={()=>setEditing(item)}><PencilIcon/></Button></div>)}</div>
    {!page.items.length&&!failed&&<Empty><EmptyHeader><EmptyMedia variant="icon"><StarIcon/></EmptyMedia><EmptyTitle>{t(query?"browser_use.no_results":"browser_use.no_bookmarks")}</EmptyTitle></EmptyHeader></Empty>}
    {page.next_offset!==null&&<Button variant="ghost" disabled={pending} onClick={()=>void more()}>{t("browser_use.load_more")}</Button>}{editing&&<BookmarkEditor draft={editing} open onClose={()=>setEditing(null)}/>}</section>;
}
