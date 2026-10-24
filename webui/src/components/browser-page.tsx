import { useEffect, useRef, useState, type PointerEvent as ReactPointerEvent } from "react";
import { useTranslation } from "react-i18next";
import { ArrowLeftIcon, ArrowRightIcon, CameraIcon, ChevronDownIcon, ChevronUpIcon, Clock3Icon, DownloadIcon, FolderOpenIcon, LoaderCircleIcon, MinusIcon, MoreHorizontalIcon, PanelLeftIcon, PlusIcon, PrinterIcon, RotateCwIcon, SearchIcon, Settings2Icon, Trash2Icon, XIcon } from "lucide-react";
import { t } from "@/lib/i18n";
import { browserControl, browserFind, browserLayout, browserLibrary, browserScreenshot, subscribeBrowserWorkspace, type BrowserAction, type BrowserFindAction, type BrowserFindResult, type BrowserLibrary, type BrowserLibraryAction, type BrowserWorkspace } from "@/lib/browser-workspace";
import { browserDestination } from "@/lib/browser-address";
import { browserShortcut } from "@/lib/browser-shortcuts";
import { isAndroid } from "@/lib/mobile";
import { Button } from "@/components/ui/button";
import { InputGroup, InputGroupAddon, InputGroupInput } from "@/components/ui/input-group";
import { Empty, EmptyHeader, EmptyMedia, EmptyTitle } from "@/components/ui/empty";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { DropdownMenu, DropdownMenuContent, DropdownMenuGroup, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import { BrowserSettings, ClearBrowserDataDialog } from "@/components/settings/browser-settings";
import { useBrowserPreferences, searchEngines } from "@/lib/browser-settings";
import { BrowserSiteIcon } from "@/components/browser-site-icon";
import { BrowserDownloads } from "@/components/browser-downloads";
import { Popover, PopoverContent, PopoverTitle, PopoverTrigger } from "@/components/ui/popover";

const initial: BrowserWorkspace = { pages: [], selected: null };
const emptyLibrary:BrowserLibrary={history:[],downloads:[]};
type Panel="history"|"downloads"|"settings"|null;
const host=(url:string)=>{try{return new URL(url).hostname;}catch{return url;}};
export function BrowserPage({ onNavigation }: { onNavigation(): void }) {
  const {i18n}=useTranslation();
  const [workspace,setWorkspace]=useState(initial);
  const [library,setLibrary]=useState(emptyLibrary);
  const [address,setAddress]=useState("");
  const [requestedPanel,setPanel]=useState<Panel>(()=>{const value=new URLSearchParams(location.search).get("view");return value==="history"||value==="downloads"||value==="settings"?value:null;});
  const [wide,setWide]=useState(()=>matchMedia("(min-width: 768px)").matches);
  const downloadPopover=wide&&requestedPanel==="downloads";
  const panel=requestedPanel==="downloads"&&wide?null:requestedPanel;
  const [query,setQuery]=useState("");
  const [find,setFind]=useState<string|null>(null);
  const [matches,setMatches]=useState<BrowserFindResult|null>(null);
  const [capturing,setCapturing]=useState(false);
  const [notice,setNotice]=useState<"clipboard"|"download"|null>(null);
  const searchVersion=useRef(0);
  const findInput=useRef<HTMLInputElement>(null);
  const findTimer=useRef<ReturnType<typeof setTimeout>|undefined>(undefined);
  const [failed,setFailed]=useState(false);
  const [busy,setBusy]=useState(false);
  const [menu,setMenu]=useState(false);
  const [clear,setClear]=useState(false);
  const [dragging,setDragging]=useState<string|null>(null);
  const [drop,setDrop]=useState<{tab:string;after:boolean}|null>(null);
  const pointer=useRef<{id:number;tab:string;x:number;y:number}|null>(null);
  const suppressClick=useRef(0);
  const dragSource=useRef<string|null>(null);
  const tabStrip=useRef<HTMLDivElement>(null);
  const [canDrag,setCanDrag]=useState(false);
  const [expanded,setExpanded]=useState(true);
  const {value:browserSettings}=useBrowserPreferences();
  const engine=searchEngines[browserSettings.search_engine];
  const viewport=useRef<HTMLDivElement>(null);
  const downloadsPopup=useRef<HTMLDivElement>(null);
  const input=useRef<HTMLInputElement>(null);
  const alive=useRef(false);
  const edited=useRef(false);
  const locking=useRef(false);
  const blocked=useRef(false);
  const current=workspace.pages.find(page=>page.tab===workspace.selected);
  const home=!current || current.url==="about:blank";
  const nativeVisible=!home && !panel;
  blocked.current=!nativeVisible;
  const zoom=current?.zoom??1;
  useEffect(()=>{if(!edited.current)setAddress(home?"":current?.url??"");},[current?.url,home]);
  async function refreshLibrary(offset=0){
    try{const value=await browserLibrary("library",String(offset));if(!alive.current)return;
      setLibrary(previous=>offset?{history:[...previous.history,...value.history.filter(item=>!previous.history.some(old=>old.id===item.id))],downloads:[...previous.downloads,...value.downloads.filter(item=>!previous.downloads.some(old=>old.id===item.id))],next_offset:value.next_offset}:value);
    }catch{if(alive.current)setFailed(true);}
  }
  useEffect(()=>{
    alive.current=true;let polling=false,again=false,frame=0,lastLayout="";
    const update=()=>{
      frame=0;const rect=viewport.current?.getBoundingClientRect();
      const overlays=Array.from(document.querySelectorAll<HTMLElement>('[data-slot="dialog-content"][data-state="open"], [data-slot="sheet-content"][data-state="open"], [data-slot="dropdown-menu-content"][data-state="open"], [data-slot="select-content"][data-state="open"], [data-slot="context-menu-content"][data-state="open"], [data-slot="popover-content"][data-state="open"]'));
      const bounds=!blocked.current && !document.hidden && rect && rect.width>0 && rect.height>0?{x:rect.x,y:rect.y,width:rect.width,height:rect.height,scale:window.devicePixelRatio}:null;
      const cuts=bounds?overlays.map(node=>{const area=node.getBoundingClientRect();return {x:area.x,y:area.y,width:area.width,height:area.height,radius:parseFloat(getComputedStyle(node).borderTopLeftRadius)||0};}).filter(area=>area.width>0&&area.height>0&&area.x<bounds.x+bounds.width&&area.y<bounds.y+bounds.height&&area.x+area.width>bounds.x&&area.y+area.height>bounds.y):[];
      if(overlays.some(node=>node.getAnimations().some(animation=>animation.playState==="running")))measure();
      const signature=JSON.stringify({bounds,cuts});if(signature===lastLayout)return;lastLayout=signature;
      void browserLayout(bounds,cuts).catch(()=>{lastLayout="";if(alive.current)setFailed(true);});
    };
    const measure=()=>{if(!frame)frame=requestAnimationFrame(update);};
    const resize=new ResizeObserver(measure);if(viewport.current)resize.observe(viewport.current);
    const overlays=new MutationObserver(measure);overlays.observe(document.body,{childList:true,subtree:true,attributes:true,attributeFilter:["data-state","data-browser-surface"]});
    const refresh=async()=>{if(document.hidden||locking.current)return;if(polling){again=true;return;}polling=true;try{const value=await browserControl("list");if(alive.current){setWorkspace(value);measure();}}catch{if(alive.current)setFailed(true);}finally{polling=false;if(again){again=false;void refresh();}}};
    let disposed=false;let unsubscribe:(()=>void)|undefined;
    void subscribeBrowserWorkspace(()=>{void refresh();}).then(stop=>{if(disposed)stop();else unsubscribe=stop;}).catch(()=>{});
    const visibility=()=>{measure();void refresh();};
    document.addEventListener("visibilitychange",visibility);window.addEventListener("resize",measure);
    const timer=setInterval(()=>{void refresh();},1000);measure();void refresh();void refreshLibrary();
    return()=>{disposed=true;unsubscribe?.();alive.current=false;clearInterval(timer);cancelAnimationFrame(frame);resize.disconnect();overlays.disconnect();document.removeEventListener("visibilitychange",visibility);window.removeEventListener("resize",measure);void browserLayout(null).catch(()=>{});};
  },[]);
  useEffect(()=>{if(panel!=="downloads" && panel!=="history" && !home && !downloadPopover)return;void refreshLibrary();const timer=setInterval(()=>{if(!document.hidden)void refreshLibrary();},4000);return()=>clearInterval(timer);},[panel,home,downloadPopover]);
  useEffect(()=>{const media=matchMedia("(min-width: 768px)");const update=()=>setWide(media.matches);media.addEventListener("change",update);return()=>media.removeEventListener("change",update);},[]);
  async function control(action:BrowserAction,tab:string|null=null,url:string|null=null){
    if(locking.current)return;locking.current=true;setBusy(true);setFailed(false);
    try{const value=await browserControl(action,tab,url);if(alive.current){setWorkspace(value);if(["open","new_tab","navigate","select","back","forward"].includes(action)||value.selected!==workspace.selected){setPanel(null);setFind(null);edited.current=false;setAddress(value.pages.find(page=>page.tab===value.selected)?.url.replace(/^about:blank$/,"")??"");}}}
    catch{if(alive.current)setFailed(true);}finally{locking.current=false;if(alive.current)setBusy(false);}
  }
  async function libraryAction(action:BrowserLibraryAction,value:string|null=null){
    setFailed(false);try{await browserLibrary(action,value);await refreshLibrary();if(action==="zoom"){const state=await browserControl("list");if(alive.current)setWorkspace(state);}}catch{if(alive.current)setFailed(true);}
  }
  async function searchPage(action:BrowserFindAction,text=find??"",tab=current?.tab){
    if(!tab)return;
    clearTimeout(findTimer.current);
    const requested=(action==="find_next"||action==="find_previous")&&!matches?"find":action;
    const version=++searchVersion.current;
    try{const result=await browserFind(requested,text,tab);if(alive.current&&searchVersion.current===version)setMatches(result);}
    catch{if(alive.current&&searchVersion.current===version)setFailed(true);}
  }
  function openFind(){setFind(value=>value??"");requestAnimationFrame(()=>{findInput.current?.focus();findInput.current?.select();});}
  function closeFind(){++searchVersion.current;setFind(null);setMatches(null);}
  useEffect(()=>{
    const tab=current?.tab;
    if(find===null||!tab)return;
    const timer=findTimer.current=setTimeout(()=>void searchPage("find",find,tab),find?150:0);
    return()=>{clearTimeout(timer);++searchVersion.current;};
  },[find,current?.tab,current?.url]);
  const finding=find!==null;
  useEffect(()=>{
    const tab=current?.tab;
    if(!finding||!tab)return;
    return()=>{void browserFind("find_close","",tab).catch(()=>{});};
  },[finding,current?.tab]);
  useEffect(()=>{if(!notice)return;const timer=setTimeout(()=>setNotice(null),3500);return()=>clearTimeout(timer);},[notice]);
  async function screenshot(){
    if(!current||capturing)return;
    setCapturing(true);setFailed(false);setNotice(null);setMenu(false);
    try{
      // Restore the actual native page after the menu has closed before capture.
      await new Promise<void>(resolve=>requestAnimationFrame(()=>requestAnimationFrame(()=>resolve())));
      const rect=viewport.current?.getBoundingClientRect();
      if(!rect)throw new Error("Browser unavailable");
      await browserLayout({x:rect.x,y:rect.y,width:rect.width,height:rect.height,scale:devicePixelRatio});
      const result=await browserScreenshot(current.tab);
      if(alive.current&&result.destination!=="cancelled")setNotice(result.destination);
    }catch{if(alive.current)setFailed(true);}finally{if(alive.current)setCapturing(false);}
  }
  function showPanel(value:Panel){setPanel(value);setQuery("");setFind(null);setMenu(false);}
  function focusAddress(){input.current?.focus();input.current?.select();}
  function selectOffset(offset:number){const index=workspace.pages.findIndex(page=>page.tab===workspace.selected);const page=workspace.pages[(index+offset+workspace.pages.length)%workspace.pages.length];if(page)void control("select",page.tab);}
  useEffect(()=>{
    const perform=(code:string)=>{
      if(document.querySelector('[data-slot="dialog-content"]'))return;
      if(code==="l")focusAddress();
      if(code==="t"){void control("new_tab");setTimeout(focusAddress,100);}
      if(code==="h")showPanel("history");
      if(code==="j")showPanel("downloads");
      if(code==="f" && !home && !panel)openFind();
      if(code==="w" && current)void control("close",current.tab);
      if(code==="r" && !home)void control("reload");
      if(code==="p" && !home)void libraryAction("print");
      if(code==="back" && current?.can_back)void control("back");
      if(code==="forward" && current?.can_forward)void control("forward");
      if(code==="next-tab")selectOffset(1);
      if(code==="previous-tab")selectOffset(-1);
      if(code.startsWith("tab-")){const number=Number(code.slice(4));const page=number===9?workspace.pages.at(-1):workspace.pages[number-1];if(page)void control("select",page.tab);}
      if(code.startsWith("zoom-")&&!home)void libraryAction("zoom",String(code==="zoom-reset"?1:Math.max(.25,Math.min(5,zoom+(code==="zoom-in"?.1:-.1)))));
      if(code==="find-next"||code==="find-previous"){if(!home&&!panel){if(find===null)openFind();else if(find)void searchPage(code==="find-next"?"find_next":"find_previous");}}
      if(code==="escape"){if(find!==null)closeFind();else if(current?.loading)void control("stop");}
    };
    const key=(event:KeyboardEvent)=>{if(event.defaultPrevented||document.querySelector('[data-slot="dialog-content"], [data-slot="dropdown-menu-content"]')||(event.key==="Escape"&&downloadPopover))return;const code=browserShortcut(event);if(!code)return;if(code==="escape"&&find===null&&!current?.loading)return;event.preventDefault();perform(code);};
    const shortcut=(event:Event)=>perform((event as CustomEvent<string>).detail);
    document.addEventListener("keydown",key);window.addEventListener("dl-browser-shortcut",shortcut);
    return()=>{document.removeEventListener("keydown",key);window.removeEventListener("dl-browser-shortcut",shortcut);};
  },[workspace,home,panel,zoom,find,matches,downloadPopover]);
  useEffect(()=>{
    const media=matchMedia("(min-width: 768px) and (hover: hover) and (pointer: fine)");
    const update=()=>setCanDrag(media.matches&&!/Android|iPhone|iPad/i.test(navigator.userAgent));
    update();media.addEventListener("change",update);return()=>media.removeEventListener("change",update);
  },[]);
  useEffect(()=>{const active=tabStrip.current?.querySelector('[aria-selected="true"]');active?.scrollIntoView({block:"nearest",inline:"nearest"});},[workspace.selected]);
  function resetDrag(){pointer.current=null;dragSource.current=null;setDragging(null);setDrop(null);}
  function dragTarget(x:number,y:number){
    const strip=tabStrip.current;if(!strip)return null;
    const bounds=strip.getBoundingClientRect();if(y<bounds.top||y>bounds.bottom||x<bounds.left||x>bounds.right)return null;
    const tabs=Array.from(strip.querySelectorAll<HTMLDivElement>('.dl-browser-tab'));
    const node=tabs.find(node=>{const rect=node.getBoundingClientRect();return x>=rect.left&&x<=rect.right;});
    if(!node)return null;
    const rect=node.getBoundingClientRect();return {tab:node.dataset.tab!,after:x>rect.left+rect.width/2};
  }
  function movePointer(event:ReactPointerEvent<HTMLDivElement>){
    const start=pointer.current;
    if(!start||start.id!==event.pointerId||!canDrag)return;
    if(!dragSource.current){
      if(Math.hypot(event.clientX-start.x,event.clientY-start.y)<6)return;
      event.currentTarget.setPointerCapture(event.pointerId);dragSource.current=start.tab;setDragging(start.tab);
    }
    event.preventDefault();const target=dragTarget(event.clientX,event.clientY);
    setDrop(old=>old?.tab===target?.tab&&old?.after===target?.after?old:target);
    const strip=tabStrip.current;
    if(strip){const bounds=strip.getBoundingClientRect();if(event.clientX<bounds.left+28)strip.scrollLeft-=24;else if(event.clientX>bounds.right-28)strip.scrollLeft+=24;}
  }
  function dropPointer(event:ReactPointerEvent<HTMLDivElement>){
    const source=dragSource.current;
    if(source){
      event.preventDefault();suppressClick.current=performance.now()+150;
      const target=dragTarget(event.clientX,event.clientY);
      if(canDrag&&target&&source!==target.tab){const rest=workspace.pages.filter(page=>page.tab!==source);const index=rest.findIndex(page=>page.tab===target.tab)+(target.after?1:0);void control("move_tab",source,rest[index]?.tab??null);}
    }
    if(event.currentTarget.hasPointerCapture(event.pointerId))event.currentTarget.releasePointerCapture(event.pointerId);
    resetDrag();
  }
  function go(value:string){const url=browserDestination(value,engine);if(!url){setFailed(true);return;}edited.current=false;input.current?.blur();void control(current?"navigate":"open",null,url);}
  const recent=library.history.filter(item=>`${item.title} ${item.url}`.toLocaleLowerCase().includes(query.toLocaleLowerCase()));
  const recommendations=library.history.filter((item,index,all)=>all.findIndex(other=>host(other.url)===host(item.url))===index).slice(0,4);
  const time=(value:number)=>new Date(value).toLocaleTimeString(i18n.language,{hour:"2-digit",minute:"2-digit"});
  const date=(value:number)=>new Date(value).toLocaleDateString(i18n.language,{month:"short",day:"numeric"});
  const title=current?.title||t("browser_use.new_tab");
  return <section className="dl-browser-page" aria-label={t("browser_use.title")} data-browser-surface={nativeVisible?"page":"local"}>
    <div className="dl-browser-tabs" role="tablist" aria-label={t("browser_use.tabs")} data-window-drag-region>
      <Button variant="ghost" size="icon-sm" className="dl-apps-mobile-navigation" aria-label={t("navigation.open_sidebar")} onClick={onNavigation}><PanelLeftIcon/></Button>
      <div ref={tabStrip} className="dl-browser-tab-strip" onDoubleClick={event=>{if(event.target===event.currentTarget)void control("new_tab");}}>{workspace.pages.map(page=><div className="dl-browser-tab" key={page.tab} data-tab={page.tab} data-window-no-drag data-active={page.tab===workspace.selected} data-dragging={dragging===page.tab||undefined} data-drop={drop?.tab===page.tab?(drop.after?"after":"before"):undefined} draggable={false} onPointerDown={event=>{if(canDrag&&!busy&&event.button===0&&event.pointerType==="mouse"&&!(event.target as HTMLElement).closest('[data-slot="button"]'))pointer.current={id:event.pointerId,tab:page.tab,x:event.clientX,y:event.clientY};}} onPointerMove={movePointer} onPointerUp={dropPointer} onPointerCancel={resetDrag} onLostPointerCapture={resetDrag} onClickCapture={event=>{if(performance.now()<suppressClick.current){event.preventDefault();event.stopPropagation();}}} onMouseDown={event=>{if(event.button===1)event.preventDefault();}} onAuxClick={event=>{if(event.button===1){event.preventDefault();event.stopPropagation();void control("close",page.tab);}}}>
        <button role="tab" type="button" tabIndex={page.tab===workspace.selected?0:-1} aria-selected={page.tab===workspace.selected} title={page.url==="about:blank"?t("browser_use.new_tab"):page.url} disabled={busy} onClick={()=>void control("select",page.tab)} onKeyDown={event=>{const code=event.key;if(!["ArrowLeft","ArrowRight","Home","End"].includes(code)||event.altKey||event.ctrlKey||event.metaKey)return;event.preventDefault();if(code==="ArrowLeft"||code==="ArrowRight")selectOffset(code==="ArrowLeft"?-1:1);else{const target=code==="Home"?workspace.pages[0]:workspace.pages.at(-1);if(target)void control("select",target.tab);}}}>{page.loading?<LoaderCircleIcon className="animate-spin"/>:<BrowserSiteIcon src={page.favicon}/>}<span>{page.title||(page.url==="about:blank"?t("browser_use.new_tab"):host(page.url))}</span></button>
        <Button variant="ghost" size="icon-sm" disabled={busy} aria-label={t("browser_use.close_tab",{title:page.title||(page.url==="about:blank"?t("browser_use.new_tab"):host(page.url))})} onClick={()=>void control("close",page.tab)}><XIcon/></Button>
      </div>)}</div>
      <Button variant="ghost" size="icon-sm" aria-label={t("browser_use.new_tab")} disabled={busy} onClick={()=>{void control("new_tab");setTimeout(focusAddress,100);}}><PlusIcon/></Button>
    </div>
    <header className="dl-browser-toolbar">
      <div className="dl-browser-navigation">
        <Button variant="ghost" size="icon" disabled={busy||!current||current.can_back===false} aria-label={t("browser_use.back")} onClick={()=>void control("back")}><ArrowLeftIcon/></Button>
        <Button variant="ghost" size="icon" disabled={busy||!current||current.can_forward===false} aria-label={t("browser_use.forward")} onClick={()=>void control("forward")}><ArrowRightIcon/></Button>
        <span className="dl-browser-toolbar-divider"/>
        <Button variant="ghost" size="icon" disabled={busy||home} aria-label={t(current?.loading?"browser_use.stop":"browser_use.reload")} onClick={()=>void control(current?.loading?"stop":"reload")}>{current?.loading?<XIcon/>:<RotateCwIcon/>}</Button>
      </div>
      <form className="dl-browser-address" onSubmit={event=>{event.preventDefault();go(address);}}><InputGroup><InputGroupInput ref={input} aria-label={t("browser_use.address")} placeholder={t("browser_use.address_hint")} value={address} onChange={event=>{edited.current=true;setAddress(event.target.value);}} onFocus={event=>event.target.select()} onKeyDown={event=>{if(event.key==="Escape"){edited.current=false;setAddress(home?"":current?.url??"");input.current?.blur();}}} spellCheck={false} autoComplete="off" autoCapitalize="off"/></InputGroup></form>
      <div className="dl-browser-actions">
        <Button variant="ghost" size="icon" aria-label={t("browser_use.history")} onClick={()=>showPanel(panel==="history"?null:"history")}><Clock3Icon/></Button>
        <Popover modal={false} open={downloadPopover} onOpenChange={open=>{if(wide)showPanel(open?"downloads":null);}}>
          <PopoverTrigger asChild><Button variant="ghost" size="icon" aria-label={t("browser_use.downloads")} onClick={()=>{if(!wide)showPanel(panel==="downloads"?null:"downloads");}}><DownloadIcon/></Button></PopoverTrigger>
          {wide && <PopoverContent ref={downloadsPopup} tabIndex={-1} onOpenAutoFocus={event=>{event.preventDefault();downloadsPopup.current?.focus();}} align="end" side="bottom" sideOffset={8} collisionPadding={12} className="dl-browser-downloads" aria-label={t("browser_use.downloads")}>
            <header><PopoverTitle>{t("browser_use.downloads")}</PopoverTitle>{!isAndroid() && <Button variant="ghost" size="icon-sm" aria-label={t("browser_use.open_downloads")} title={t("browser_use.open_downloads")} onClick={()=>void libraryAction("open_downloads")}><FolderOpenIcon/></Button>}</header>
            <div className="dl-browser-download-scroll"><BrowserDownloads library={library} popup onAction={(action,value)=>void libraryAction(action,value)} onMore={()=>void refreshLibrary(library.next_offset!)}/></div>
          </PopoverContent>}
        </Popover>
      </div>
      <DropdownMenu modal={false} open={menu} onOpenChange={setMenu}><DropdownMenuTrigger asChild><Button variant="ghost" size="icon" className="dl-browser-more" aria-label={t("browser_use.menu")}><MoreHorizontalIcon/></Button></DropdownMenuTrigger><DropdownMenuContent align="end" sideOffset={8} className="dl-browser-menu" onCloseAutoFocus={event=>{if(downloadPopover){event.preventDefault();downloadsPopup.current?.focus();}}}><DropdownMenuGroup>
        <DropdownMenuItem onSelect={()=>void control("new_tab")}><PlusIcon/>{t("browser_use.new_tab")}<kbd>Ctrl+T</kbd></DropdownMenuItem>
        <DropdownMenuItem disabled={home||Boolean(panel)} onSelect={openFind}><SearchIcon/>{t("browser_use.find")}<kbd>Ctrl+F</kbd></DropdownMenuItem>
        <DropdownMenuItem disabled={home||Boolean(panel)||capturing} onSelect={()=>void screenshot()}><CameraIcon/>{t("browser_use.screenshot")}</DropdownMenuItem>
        <DropdownMenuItem disabled={home} onSelect={()=>void libraryAction("print")}><PrinterIcon/>{t("browser_use.print")}<kbd>Ctrl+P</kbd></DropdownMenuItem>
      </DropdownMenuGroup><DropdownMenuSeparator/>
        <div className="dl-browser-zoom"><span>{t("browser_use.zoom")}</span><div><Button variant="ghost" size="icon-sm" aria-label={t("browser_use.zoom_out")} disabled={home||zoom<=0.25} onClick={()=>void libraryAction("zoom",String(Math.max(.25,zoom-.1)))}><MinusIcon/></Button><Button variant="ghost" size="sm" disabled={home} onClick={()=>void libraryAction("zoom","1")}>{Math.round(zoom*100)}%</Button><Button variant="ghost" size="icon-sm" aria-label={t("browser_use.zoom_in")} disabled={home||zoom>=5} onClick={()=>void libraryAction("zoom",String(Math.min(5,zoom+.1)))}><PlusIcon/></Button></div></div>
      <DropdownMenuSeparator/><DropdownMenuGroup><DropdownMenuItem onSelect={()=>showPanel("downloads")}><DownloadIcon/>{t("browser_use.downloads")}<kbd>Ctrl+J</kbd></DropdownMenuItem><DropdownMenuItem onSelect={()=>showPanel("history")}><Clock3Icon/>{t("browser_use.history")}<kbd>Ctrl+H</kbd></DropdownMenuItem><DropdownMenuItem onSelect={()=>setClear(true)}><Trash2Icon/>{t("browser_use.clear_data")}</DropdownMenuItem></DropdownMenuGroup><DropdownMenuSeparator/><DropdownMenuGroup><DropdownMenuItem onSelect={()=>showPanel("settings")}><Settings2Icon/>{t("browser_use.settings")}</DropdownMenuItem></DropdownMenuGroup></DropdownMenuContent></DropdownMenu>
    </header>
    <div className="dl-browser-progress" data-loading={Boolean(current?.loading)&&!panel} role={current?.loading?"progressbar":undefined} aria-label={current?.loading?t("browser_settings.loading"):undefined} aria-valuemin={0} aria-valuemax={100} aria-valuenow={current?.loading?current.progress:undefined}><span style={current?.progress!==undefined?{width:`${current.progress}%`}:undefined}/></div>
    {failed && <Alert variant="destructive" className="dl-browser-error"><AlertDescription>{t("browser_use.unavailable")}</AlertDescription><Button variant="ghost" size="icon-sm" aria-label={t("common.close")} onClick={()=>setFailed(false)}><XIcon/></Button></Alert>}
    {notice && <div className="dl-browser-notice" role="status">{t(`browser_use.screenshot_${notice}`)}</div>}
    {find!==null && <form className="dl-browser-find" onSubmit={event=>{event.preventDefault();if(find)void searchPage("find_next");}}><InputGroup><InputGroupInput ref={findInput} autoFocus aria-label={t("browser_use.find")} value={find} aria-invalid={Boolean(find&&matches&&!matches.found)} onChange={event=>{++searchVersion.current;setMatches(null);setFind(event.target.value);}} onKeyDown={event=>{if(event.key==="Escape"){event.preventDefault();closeFind();}if(event.key==="Enter"&&event.shiftKey){event.preventDefault();if(find)void searchPage("find_previous");}}}/><InputGroupAddon align="inline-end"><span role="status">{find&&matches?(matches.count!==null?`${matches.index} / ${matches.count}`:!matches.found?t("browser_use.no_matches"):""):""}</span></InputGroupAddon></InputGroup><Button type="button" variant="ghost" size="icon-sm" disabled={!find||!matches?.found} aria-label={t("browser_use.previous_match")} onClick={()=>void searchPage("find_previous")}><ChevronUpIcon/></Button><Button variant="ghost" size="icon-sm" disabled={!find||!matches?.found} aria-label={t("browser_use.next_match")}><ChevronDownIcon/></Button><Button type="button" variant="ghost" size="icon-sm" aria-label={t("common.close")} onClick={closeFind}><XIcon/></Button></form>}
    <div ref={viewport} className="dl-browser-viewport" data-title={title}>
      {(home||panel) && <div className="dl-browser-local"><main className="dl-browser-home">
        {panel && <div className="dl-browser-panel-heading"><Button variant="ghost" size="icon-sm" aria-label={t("browser_use.back")} onClick={()=>showPanel(null)}><ArrowLeftIcon/></Button><h1>{t(`browser_use.${panel}`)}</h1>{panel==="history"&&<Button variant="ghost" size="sm" disabled={!library.history.length} onClick={()=>void libraryAction("clear_history")}>{t("browser_use.clear_history")}</Button>}</div>}
        {!panel && <><section><h2>{t("browser_use.tools")}</h2><div className="dl-browser-tool-grid">{[
          {Icon:PlusIcon,label:"new_tab",action:()=>{void control("new_tab");setTimeout(focusAddress,100);},key:"Ctrl+T"},
          {Icon:Clock3Icon,label:"history",action:()=>showPanel("history"),key:"Ctrl+H"},
          {Icon:DownloadIcon,label:"downloads",action:()=>showPanel("downloads"),key:"Ctrl+J"},
          {Icon:Settings2Icon,label:"settings",action:()=>showPanel("settings"),key:""},
        ].map(({Icon,label,action,key})=><button type="button" key={label} onClick={action}><Icon/><span>{t(`browser_use.${label}`)}</span>{key&&<kbd>{key}</kbd>}</button>)}</div></section>
        {recommendations.length>0 && <section><h2>{t("browser_use.recommended")}</h2><div className="dl-browser-recommendations">{recommendations.map(item=><button type="button" key={item.id} onClick={()=>go(item.url)} title={item.url}><div className="dl-browser-site-icon" data-host={host(item.url)}><BrowserSiteIcon src={item.favicon}/></div><span>{item.title||host(item.url)}</span></button>)}</div></section>}</>}
        {(panel==="history" || !panel) && <section><div className="dl-browser-section-heading"><h2>{t(panel?"browser_use.history":"browser_use.recent")}</h2>{!panel&&<Button variant="ghost" size="icon-sm" aria-expanded={expanded} aria-label={t("browser_use.recent")} onClick={()=>setExpanded(value=>!value)}>{expanded?<ChevronUpIcon/>:<ChevronDownIcon/>}</Button>}</div>
          {panel && <InputGroup><InputGroupAddon><SearchIcon/></InputGroupAddon><InputGroupInput aria-label={t("browser_use.search_history")} placeholder={t("browser_use.search_history")} value={query} onChange={event=>setQuery(event.target.value)}/></InputGroup>}
          {(panel||expanded) && <div className="dl-browser-recent">{recent.slice(0,panel?undefined:6).map(item=><div key={item.id} className="dl-browser-history-row"><button type="button" onClick={()=>go(item.url)}><span className="dl-browser-row-icon"><BrowserSiteIcon src={item.favicon}/></span><span><strong>{item.title||host(item.url)}</strong><small>{panel?item.url:t("browser_use.website")}</small></span><time dateTime={new Date(item.time).toISOString()}>{panel?`${date(item.time)} · `:""}{time(item.time)}</time></button>{panel&&<Button variant="ghost" size="icon-sm" aria-label={t("browser_use.remove_history")} onClick={()=>void libraryAction("remove_history",item.id)}><XIcon/></Button>}</div>)}</div>}
          {recent.length===0 && <Empty><EmptyHeader><EmptyMedia variant="icon"><Clock3Icon/></EmptyMedia><EmptyTitle>{t(query?"browser_use.no_results":"browser_use.no_history")}</EmptyTitle></EmptyHeader></Empty>}
          {panel && library.next_offset!=null && <Button variant="ghost" onClick={()=>void refreshLibrary(library.next_offset!)}>{t("browser_use.load_more")}</Button>}
        </section>}
        {panel==="downloads" && <BrowserDownloads library={library} onAction={(action,value)=>void libraryAction(action,value)} onMore={()=>void refreshLibrary(library.next_offset!)}/>}
        {panel==="settings" && <BrowserSettings/>}
      </main></div>}
    </div>
    <ClearBrowserDataDialog open={clear} onOpenChange={setClear}/>
  </section>;
}
