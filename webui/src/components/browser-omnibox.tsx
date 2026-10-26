import {useEffect,useId,useRef,useState,type RefObject,type ReactNode} from "react";
import {Clock3Icon,SearchIcon,StarIcon,PanelsTopLeftIcon,XIcon,ArrowUpLeftIcon} from "lucide-react";
import {t} from "@/lib/i18n";
import {browserSaved,browserLibrary,type BrowserSuggestions,type BrowserPageInfo} from "@/lib/browser-workspace";
import {addressSuggestions,type AddressSuggestion,type SuggestionFilter} from "@/lib/browser-suggestions";
import {browserDestination} from "@/lib/browser-address";
import {InputGroup,InputGroupInput,InputGroupAddon} from "@/components/ui/input-group";
import {Popover,PopoverAnchor,PopoverContent} from "@/components/ui/popover";
import {Button} from "@/components/ui/button";
import {BrowserSiteIcon} from "./browser-site-icon";

const empty:BrowserSuggestions={history:[],bookmarks:[],searches:[]};
export function BrowserOmnibox({value,onChange,onNavigate,onSelectTab,onReset,input,engine,pages,current,children}:{value:string;onChange(value:string):void;onNavigate(value:string):void;onSelectTab(tab:string):void;onReset():void;input:RefObject<HTMLInputElement|null>;engine:string;pages:BrowserPageInfo[];current?:BrowserPageInfo;children?:ReactNode}){
  const [open,setOpen]=useState(false),[dirty,setDirty]=useState(false),[filter,setFilter]=useState<SuggestionFilter>(null);
  const [data,setData]=useState(empty),[selection,setSelection]=useState<string|null>(null),[revision,setRevision]=useState(0),[failed,setFailed]=useState(false);
  const popup=useRef<HTMLDivElement>(null),form=useRef<HTMLFormElement>(null),composing=useRef(false);
  const listId=useId(),query=dirty?value:"";
  const items=addressSuggestions(query,filter,data,pages,engine);
  const selected=items.findIndex(item=>item.key===selection);
  const setSelected=(index:number)=>setSelection(items[index]?.key??null);
  useEffect(()=>{if(!open)return;let live=true;setSelection(null);const timer=setTimeout(()=>{
    void browserSaved<BrowserSuggestions>("suggestions",{query}).then(value=>{if(live){setData(value);setFailed(false);}}).catch(()=>{if(live)setFailed(true);});
  },query?90:0);return()=>{live=false;clearTimeout(timer);};},[open,query,revision]);
  useEffect(()=>{const changed=()=>setRevision(value=>value+1);window.addEventListener("dl-browser-saved-changed",changed);return()=>window.removeEventListener("dl-browser-saved-changed",changed);},[]);
  useEffect(()=>{setOpen(false);setDirty(false);},[current?.tab]);
  function choose(item?:AddressSuggestion){
    if(composing.current)return;
    const destination=item?.value??value;
    if(item?.kind==="tab"&&item.tab){setOpen(false);input.current?.blur();onSelectTab(item.tab);return;}
    if(!browserDestination(destination,engine))return;
    if(browserDestination(destination,engine)===engine+encodeURIComponent(destination.trim()))void browserSaved("record_search",{query:destination.trim()}).catch(()=>{});
    setOpen(false);setDirty(false);onNavigate(destination);
  }
  async function remove(item:AddressSuggestion){
    try{if(item.kind==="history")await browserLibrary("remove_history",item.id!);else await browserSaved("remove_search",{id:item.id});setRevision(value=>value+1);setSelected(-1);}catch{setFailed(true);}
  }
  function move(offset:number){setOpen(true);setSelection(previous=>{const before=items.findIndex(item=>item.key===previous);const index=before<0?(offset<0?items.length-1:0):(before+offset+items.length)%items.length;requestAnimationFrame(()=>document.getElementById(`${listId}-${index}`)?.scrollIntoView({block:"nearest"}));return items[index]?.key??null;});}
  return <Popover modal={false} open={open} onOpenChange={setOpen}>
    <PopoverAnchor asChild><form ref={form} className="dl-browser-address" data-open={open||undefined} onSubmit={event=>{event.preventDefault();choose(selected>=0?items[selected]:undefined);}}>
      <InputGroup><InputGroupAddon>{dirty?<SearchIcon/>:<BrowserSiteIcon src={current?.favicon}/>}</InputGroupAddon><InputGroupInput ref={input} role="combobox" aria-autocomplete="list" aria-haspopup="grid" aria-controls={open?listId:undefined} aria-expanded={open} aria-activedescendant={open&&selected>=0?`${listId}-${selected}`:undefined} aria-label={t("browser_use.address")} placeholder={t("browser_use.address_hint")} value={value} onChange={event=>{setDirty(true);setOpen(true);setSelected(-1);onChange(event.target.value);}} onFocus={event=>{if(!open){event.target.select();setDirty(value!==(current?.url==="about:blank"?"":current?.url??""));setFilter(null);setSelected(-1);setOpen(true);}}} onBlur={()=>{setTimeout(()=>{if(!popup.current?.contains(document.activeElement)&&!form.current?.contains(document.activeElement))setOpen(false);},0);}} onCompositionStart={()=>{composing.current=true;}} onCompositionEnd={()=>{composing.current=false;}} onKeyDown={event=>{
        if(event.nativeEvent.isComposing||composing.current)return;
        if((event.key==="ArrowDown"||event.key==="ArrowUp")&&items.length){event.preventDefault();move(event.key==="ArrowDown"?1:-1);}
        if(event.key==="Delete"&&event.shiftKey&&selected>=0&&["history","search_history"].includes(items[selected]?.kind)){event.preventDefault();void remove(items[selected]);}
        if(event.key==="Escape"){event.preventDefault();event.stopPropagation();if(open)setOpen(false);else{onReset();input.current?.blur();}}
      }} spellCheck={false} autoComplete="off" autoCapitalize="off"/>{children&&<InputGroupAddon align="inline-end">{children}</InputGroupAddon>}</InputGroup>
    </form></PopoverAnchor>
    <PopoverContent ref={popup} align="start" side="bottom" sideOffset={0} avoidCollisions={false} className="dl-browser-suggestions" onOpenAutoFocus={event=>event.preventDefault()} onCloseAutoFocus={event=>event.preventDefault()} onInteractOutside={event=>{if(form.current?.contains(event.target as Node))event.preventDefault();}}>
      <div className="dl-browser-suggestion-scroll"><div className="dl-browser-suggestion-heading">{t(!query&&!filter&&data.searches.length?"browser_use.recent_searches":"browser_use.suggestions")}</div>
      <div id={listId} role="grid" aria-label={t("browser_use.suggestions")}>
        {items.map((item,index)=><div key={item.key} id={`${listId}-${index}`} role="row" aria-selected={selected===index} className="dl-browser-suggestion-row" onPointerMove={()=>setSelected(index)}>
          <div role="gridcell" className="dl-browser-suggestion-cell"><button type="button" tabIndex={-1} onPointerDown={event=>event.preventDefault()} onClick={()=>choose(item)}>
            {item.kind==="search"?<SearchIcon/>:item.kind==="search_history"||item.kind==="history"?<Clock3Icon/>:item.kind==="bookmark"?<StarIcon/>:<BrowserSiteIcon src={item.favicon}/>}
            <span><span className="dl-browser-suggestion-label">{item.label}</span>{item.detail&&item.kind!=="search"&&<small>{item.detail}</small>}</span>
            {item.kind==="search"&&<small>{t(item.detail==="search"?"browser_use.search_action":"browser_use.go")}</small>}{item.kind==="tab"&&<small>{t("browser_use.switch_tab")}</small>}
          </button></div>
          <div role="gridcell">{["history","search_history"].includes(item.kind)?<Button variant="ghost" size="icon-sm" aria-label={t("browser_use.remove_suggestion",{title:item.label})} onPointerDown={event=>event.preventDefault()} onClick={()=>void remove(item)}><XIcon/></Button>:<ArrowUpLeftIcon className="dl-browser-suggestion-arrow"/>}</div>
        </div>)}
      </div>{!items.length&&<div className="dl-browser-suggestion-empty" role="status">{t("browser_use.no_results")}</div>}{failed&&<div className="dl-browser-suggestion-empty" role="status">{t("browser_use.suggestions_failed")}</div>}</div>
      <footer><span>{t("browser_use.filter_search")}</span>{([{value:"history",Icon:Clock3Icon},{value:"bookmarks",Icon:StarIcon},{value:"tabs",Icon:PanelsTopLeftIcon}] as const).map(({value,Icon})=><Button key={value} variant="ghost" size="sm" aria-pressed={filter===value} onPointerDown={event=>event.preventDefault()} onClick={()=>{setFilter(previous=>previous===value?null:value);setSelected(-1);input.current?.focus();}}><Icon data-icon="inline-start"/>{t(`browser_use.${value}`)}</Button>)}</footer>
    </PopoverContent>
  </Popover>;
}
