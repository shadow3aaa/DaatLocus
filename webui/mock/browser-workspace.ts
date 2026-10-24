/** Development-only browser fixtures; no native bridge or external page requests. */
import type { BrowserAction, BrowserBounds, BrowserWorkspace, BrowserLibraryAction, BrowserLibrary, BrowserFindAction, BrowserFindResult, BrowserPreferences } from "../src/lib/browser-workspace";
import cursorScript from "../../crates/daat-locus-client-protocol/src/browser-cursor.js?raw";
import html2canvas from "html2canvas";
import { browserShortcut } from "../src/lib/browser-shortcuts";
import backgroundScript from "../../clients/app/src-tauri/src/browser-background.js?raw";
const listeners=new Set<()=>void>();
export async function subscribeBrowserWorkspace(callback:()=>void){listeners.add(callback);return()=>{listeners.delete(callback);};}
const changed=()=>listeners.forEach(callback=>callback());
let preferences:BrowserPreferences=JSON.parse(localStorage.getItem("dl.mock.browser.preferences")??'null')??{search_engine:"google",language:"system",default_zoom:1};
export async function browserPreferences(patch?:Partial<BrowserPreferences>){if(patch){preferences={...preferences,...patch};localStorage.setItem("dl.mock.browser.preferences",JSON.stringify(preferences));}return structuredClone(preferences);}
const icon="data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAYAAABzenr0AAABsUlEQVR4nGNgGGDAiE/SveLWf2pZtLNDDatdjLS2mJBDmOhpOTbzmehpOTZ7mOhtObp9TAwDDBiRXTMQgIUYRd7m/Ay5AWIY4v//MzD8+P2P4d2nvwzXHn5n2HDsA8PdZz+p7wBcgJGRgYGTjYlBWgSEWRmcDfkYZm59zbDx2AcGYgET0SqJMYyJgSHTV5TBQpObdiFw9eF3huIZT8C+Z2ZiZODnZmYw1+BmSHQXZuDlYgarATni1M2vDP/+EeFoBjIBKP7//P3P8PbTH4Ztpz4yNC15DpcTF2RlMFHjpm8UXL7/neEOUgLUVeSkrwNA4NbjHwwwICvKxkB3B3z+joh0bg7ijGZioBEAJVC6O4CXE2Hcl+9/6e8AJUl2OPvJm9/0dYCcGBuDuiwHnH/p3jf6FMXcHEwMpurcDPFuwmA+CLx4/xtcENHEAdrynAw72lXxqpm15Q1RpSBZDsAHQJbO2f6G4di1L0TrYaHEQlBx/PP3P4bXH/8wXLr3nWHz8Q8MD17+GloNEqaBshjFATtxdBpoDUD2Do4QGIhQgNnHhE2QXpZjOIAejkA3f8B7xwBtGZQ3QE+2wAAAAABJRU5ErkJggg==";
export const browserSupported = () => true;
const demo="https://example.com/browser-demo";
let state: BrowserWorkspace={pages:[{tab:"home",url:"about:blank",title:""},{tab:"demo",url:demo,title:"光标与输入演示",can_back:false,can_forward:false}],selected:"home"};
const library:BrowserLibrary={history:[
  {id:"demo",url:demo,title:"光标与输入演示",time:Date.now()-60000},
  {id:"github",url:"https://github.com/",title:"GitHub",time:Date.now()-120000},
  {id:"mdn",url:"https://developer.mozilla.org/",title:"MDN Web Docs",time:Date.now()-240000},
  {id:"cloudflare",url:"https://dash.cloudflare.com/",title:"Cloudflare Dashboard",time:Date.now()-360000},
  {id:"wiki",url:"https://www.wikipedia.org/",title:"Wikipedia",time:Date.now()-86400000},
],downloads:[{id:"guide",url:demo,name:"browser-guide.txt",time:Date.now()-120000,status:"complete",bytes:2048},{id:"interrupted",url:demo,name:"archive.zip",time:Date.now()-180000,status:"interrupted",bytes:131072},{id:"failed",url:demo,name:"report.pdf",time:Date.now()-240000,status:"failed",bytes:0}],next_offset:null};
const histories=new Map<string,{urls:string[],index:number}>();
const scene=new URLSearchParams(location.search).get("browser_scene");
if(scene==="empty"){state={pages:[],selected:null};library.history=[];library.downloads=[];}
if(scene==="cursor")state.selected="demo";
if(scene==="tint"){state.selected="demo";state.pages[1].background="#1c3557";}
if(scene==="downloads"){
  state.selected="demo";
  library.downloads=[
    {id:"image",name:"unnamed.png",status:"complete" as const,bytes:37888},
    {id:"large",name:"DaatLocus-windows-x86_64.zip",status:"downloading" as const,bytes:25165824},
    {id:"failed-image",name:"lx32mlzq-11.png",status:"failed" as const,bytes:161894},
    {id:"interrupted-image",name:"lx32meu7-10.png",status:"interrupted" as const,bytes:43213},
    ...Array.from({length:12},(_,index)=>({id:`photo-${index}`,name:`screenshot-${index+1}.png`,status:"complete" as const,bytes:20480+index*1820})),
  ].map((item,index)=>({...item,url:demo,time:Date.now()-index*60000}));
}
for(const item of library.history)item.favicon=icon;
for(const page of state.pages)if(page.url!=="about:blank")page.favicon=icon;
if(scene==="loading"){state.selected="demo";state.pages[1].loading=true;setTimeout(()=>{state.pages[1].loading=false;changed();},12000);}
let active=false;
const frames=new Map<string,HTMLIFrameElement>();
let frame:HTMLIFrameElement|null=null;
function render(){
  const selected=state.pages.find(page=>page.tab===state.selected);
  for(const [tab,view] of frames){const display=active && selected?.tab===tab && selected.url!=="about:blank"?"block":"none";if(view.style.display!==display)view.style.display=display;}
  if(!active || !selected || selected.url==="about:blank")return;
  const viewport=document.querySelector(".dl-browser-viewport");if(!viewport)return;
  frame=frames.get(selected.tab)??null;
  if(!frame || !frame.isConnected){frame=document.createElement("iframe");frame.title="Browser preview";frame.setAttribute("sandbox","allow-scripts allow-downloads allow-modals");Object.assign(frame.style,{position:"absolute",inset:"0",width:"100%",height:"100%",border:"0"});viewport.append(frame);frames.set(selected.tab,frame);}
  if(frame.style.display!=="block")frame.style.display="block";
  const initialBackground=frame.dataset.background??selected.background??"";
  frame.dataset.background=initialBackground;
  const escaped=selected.title.replaceAll("&","&amp;").replaceAll("<","&lt;");
  const source=`<!doctype html><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><style>:root{color-scheme:light dark;font:16px system-ui}body{margin:0;padding:clamp(20px,5vw,64px);line-height:1.6;background:light-dark(#fff,#212121);color:light-dark(#171717,#eee)}main{max-width:760px;margin:auto}h1{font-size:28px;font-weight:600;margin:0 0 16px}p{color:light-dark(#666,#aaa)}nav{display:flex;flex-wrap:wrap;gap:8px;margin:24px 0}button,input{font:inherit;padding:9px 14px;border:1px solid #8886;border-radius:9px;background:transparent;color:inherit}button{cursor:pointer}button:hover{background:#8882}.target{min-height:190px;border:1px dashed #8888;border-radius:16px;display:grid;place-content:center;gap:12px;margin:24px 0}.target button{background:#5267df;color:white;border:0}input{width:min(320px,70vw)}output{min-height:24px;color:#5267df;display:block}section{margin:60px 0}</style><main><h1>${escaped || "Example Domain"}</h1><p>在下方选择一种操作，查看虚拟光标与动效。也可以直接点击、输入和滚动。</p><nav aria-label="光标演示">${["移动","点击","双击","拖动","滚动","键盘","连续播放","停止","边缘"].map((label,index)=>`<button data-demo="${index}">${label}</button>`).join("")}</nav><label><input id="reduced" type="checkbox" style="width:auto"> 减少动态效果</label><div class="target"><button id="target">点击按钮</button><input id="field" placeholder="试着输入文字" aria-label="演示输入"><output id="status" aria-live="polite"></output></div><button id="download">下载示例文件</button><section><h2>滚动区域</h2><p>光标不拦截鼠标事件，不改变输入焦点，也不会显示输入内容。</p></section><div style="height:650px;background:linear-gradient(transparent,#8881);border-radius:16px"></div></main><script>
const readBackground=()=>${backgroundScript};
const reportBackground=()=>parent.postMessage({type:'dl-browser-background',background:readBackground()},'*');
if(${JSON.stringify(initialBackground)})document.body.style.background=${JSON.stringify(initialBackground)};
const palette=document.createElement('nav');palette.setAttribute('aria-label','页面背景');
for(const [color,label] of [['#ffffff','浅色'],['#171717','深色'],['#1c3557','蓝色'],['#c88756','暖色']]){const button=document.createElement('button');button.textContent=label;button.onclick=()=>{document.body.style.background=color;document.body.style.color=['#171717','#1c3557'].includes(color)?'#fff':'#171717';reportBackground()};palette.append(button)}
document.querySelector('main').prepend(palette);reportBackground();
const nativeMatch=window.matchMedia.bind(window);let reduced=${new URLSearchParams(location.search).get('motion')==='full'?'false':new URLSearchParams(location.search).get('motion')==='reduce'?'true':"nativeMatch('(prefers-reduced-motion: reduce)').matches"};
window.matchMedia=query=>query==='(prefers-reduced-motion: reduce)'?{matches:reduced}:nativeMatch(query);
const cursor=(${cursorScript});let timer=0,step=0,ranges=[],match=-1;
const shortcut=(${browserShortcut.toString()});
document.addEventListener('keydown',event=>{if(!event.isTrusted||event.defaultPrevented)return;const code=shortcut(event);if(code&&code!=='escape'){event.preventDefault();parent.postMessage({type:'dl-browser-shortcut',code},'*');}});
const highlight=document.createElement('style');highlight.textContent='::highlight(dl-search){background:#ffe780;color:#171717}::selection{background:#ffb95b;color:#171717}';document.head.append(highlight);
addEventListener('message',async event=>{if(event.source!==parent||event.data?.type!=='dl-browser-command')return;const {action,value,id}=event.data;try{
if(action==='print')print();
else if(action==='screenshot'){
 const copy=document.documentElement.cloneNode(true);copy.querySelectorAll('script,dl-agent-pointer').forEach(node=>node.remove());
 copy.querySelectorAll('input').forEach((node,index)=>{node.setAttribute('value',document.querySelectorAll('input')[index].value);if(document.querySelectorAll('input')[index].checked)node.setAttribute('checked','');else node.removeAttribute('checked');});
 parent.postMessage({type:'dl-browser-result',id,result:{html:'<!doctype html>'+copy.outerHTML,width:innerWidth,height:innerHeight,x:scrollX,y:scrollY}},'*');
}else if(action.startsWith('find')){
 if(action==='find_close'||!value){ranges=[];match=-1;CSS.highlights?.delete('dl-search');getSelection()?.removeAllRanges();}
 else{
  if(action==='find'){
   ranges=[];match=-1;const walker=document.createTreeWalker(document.body,NodeFilter.SHOW_TEXT);let node;
   while(node=walker.nextNode()){if(node.parentElement.closest('script,style,dl-agent-pointer')||!node.parentElement.getClientRects().length)continue;const text=node.textContent.toLocaleLowerCase(),query=value.toLocaleLowerCase();let index=0;
    while((index=text.indexOf(query,index))>=0){const range=document.createRange();range.setStart(node,index);range.setEnd(node,index+value.length);ranges.push(range);index+=value.length;}
   }
   if(CSS.highlights)CSS.highlights.set('dl-search',new Highlight(...ranges));
  }
  if(ranges.length){match=(match+(action==='find_previous'?-1:1)+ranges.length)%ranges.length;getSelection()?.removeAllRanges();getSelection()?.addRange(ranges[match]);ranges[match].startContainer.parentElement.scrollIntoView({block:'center',behavior:'instant'});}
 }
 parent.postMessage({type:'dl-browser-result',id,result:{found:ranges.length>0,count:ranges.length,index:match+1}},'*');
}
}catch(error){parent.postMessage({type:'dl-browser-result',id,error:String(error)},'*');}});
const target=document.querySelector('#target'),status=document.querySelector('#status');
function run(index){if(index===8){cursor({kind:'move',x:innerWidth-2,y:innerHeight-2});return;}const rect=target.getBoundingClientRect(),x=rect.left+rect.width/2,y=rect.top+rect.height/2;const events=[{kind:'move',x,y},{kind:'click',x,y,count:1},{kind:'click',x,y,count:2},{kind:'drag',x,y,to_x:Math.min(innerWidth-8,x+140),to_y:y+70},{kind:'scroll',x,y,delta_x:0,delta_y:180},{kind:'key',x,y}];cursor(events[index]);}
document.querySelectorAll('[data-demo]').forEach(button=>button.onclick=()=>{const index=Number(button.dataset.demo);clearInterval(timer);if(index===7){cursor({kind:'hide'});return;}if(index===6){run(step++%6);timer=setInterval(()=>{if(!document.hidden)run(step++%6)},2200);}else run(index);});
document.querySelector('#reduced').checked=reduced;
document.querySelector('#reduced').onchange=event=>reduced=event.target.checked;
let count=0;target.onclick=()=>status.textContent='已点击 '+(++count)+' 次';
document.querySelector('#field').oninput=()=>status.textContent='已输入';
document.querySelector('#download').onclick=()=>parent.postMessage({type:'dl-browser-download'},'*');
</script>`;
  if(frame.srcdoc!==source)frame.srcdoc=source;
}
window.addEventListener("message",event=>{if(event.source===frame?.contentWindow && event.data?.type==="dl-browser-download"){library.downloads.unshift({id:crypto.randomUUID(),url:demo,name:"example.txt",time:Date.now(),status:"downloading",bytes:0});setTimeout(()=>{const item=library.downloads.find(item=>item.status==="downloading");if(item){item.status="complete";item.bytes=128;}},4000);}});
window.addEventListener("message",event=>{if(event.source===frame?.contentWindow&&event.data?.type==="dl-browser-shortcut"&&typeof event.data.code==="string")window.dispatchEvent(new CustomEvent("dl-browser-shortcut",{detail:event.data.code}));});
window.addEventListener("message",event=>{
  if(event.data?.type!=="dl-browser-background"||typeof event.data.background!=="string"||!/^#[0-9a-f]{6}$/i.test(event.data.background))return;
  const tab=[...frames].find(([,view])=>view.contentWindow===event.source)?.[0];
  const page=state.pages.find(page=>page.tab===tab);
  if(page&&page.background!==event.data.background){page.background=event.data.background;changed();}
});
let layoutOperations:Promise<unknown>=Promise.resolve();
export function browserLayout(bounds:BrowserBounds|null,_foreground:unknown[]=[]){
  const result=layoutOperations.catch(()=>{}).then(()=>{active=Boolean(bounds);render();});layoutOperations=result;return result;
}
export async function browserControl(action:BrowserAction,tab:string|null=null,url:string|null=null){
  const page=state.pages.find(page=>page.tab===state.selected);
  if(action==="select")state.selected=tab;
  if(action==="reload" && page){frames.get(page.tab)?.remove();frames.delete(page.tab);}
  if(action==="move_tab"){
    const source=state.pages.find(item=>item.tab===tab);
    if(!source||url&&!state.pages.some(item=>item.tab===url))throw new Error("Browser page closed");
    if(tab!==url){state.pages=state.pages.filter(item=>item.tab!==tab);const index=url?state.pages.findIndex(item=>item.tab===url):state.pages.length;state.pages.splice(index,0,source);}
  }
  if(action==="close"){const id=tab??state.selected;if(id){frames.get(id)?.remove();frames.delete(id);}}
  if(action==="close"){const index=state.pages.findIndex(page=>page.tab===(tab??state.selected));state.pages=state.pages.filter(page=>page.tab!==(tab??state.selected));if(!state.pages.some(page=>page.tab===state.selected))state.selected=state.pages[Math.min(index,state.pages.length-1)]?.tab??null;}
  if(action==="open" || action==="new_tab"){const next={tab:crypto.randomUUID(),url:url??"about:blank",title:url?new URL(url).hostname:"",zoom:preferences.default_zoom};state.pages.push(next);state.selected=next.tab;}
  const current=state.pages.find(page=>page.tab===state.selected);
  if(action==="navigate" && current && url){let history=histories.get(current.tab);if(!history){history={urls:[current.url],index:0};histories.set(current.tab,history);}history.urls.splice(history.index+1);history.urls.push(url);history.index++;current.url=url;current.title=new URL(url).hostname;}
  if(page && (action==="back" || action==="forward")){const history=histories.get(page.tab);if(history){history.index=Math.max(0,Math.min(history.urls.length-1,history.index+(action==="back"?-1:1)));page.url=history.urls[history.index];page.title=page.url==="about:blank"?"":new URL(page.url).hostname;}}
  if(current && ["open","navigate","reload","back","forward"].includes(action)){current.loading=true;setTimeout(()=>{current.loading=false;current.favicon=icon;changed();},1800);if(current.url!=="about:blank")library.history.unshift({id:crypto.randomUUID(),url:current.url,title:current.title,time:Date.now()});}
  for(const item of state.pages){const history=histories.get(item.tab);item.can_back=Boolean(history && history.index>0);item.can_forward=Boolean(history && history.index<history.urls.length-1);}
  render();return structuredClone(state);
}
export async function browserLibrary(action:BrowserLibraryAction,value:string|null=null):Promise<BrowserLibrary>{
  if(action==="clear_history")library.history=[];
  if(action==="clear_data")for(const view of frames.values()){view.srcdoc="";}
  if(action==="remove_history")library.history=library.history.filter(item=>item.id!==value);
  if(action==="remove_download")library.downloads=library.downloads.filter(item=>item.id!==value || item.status==="downloading");
  if(action==="save_download"){const item=library.downloads.find(item=>item.id===value);if(item?.status==="complete"){const url=URL.createObjectURL(new Blob(["Daat Locus browser download example\n".padEnd(item.bytes," ")],{type:"text/plain"}));const link=document.createElement("a");link.href=url;link.download=item.name;link.click();setTimeout(()=>URL.revokeObjectURL(url),1000);}}
  if(action==="print")frame?.contentWindow?.postMessage({type:"dl-browser-command",action,value},"*");
  if(action==="zoom"){const page=state.pages.find(page=>page.tab===state.selected);if(page)page.zoom=Number(value);if(frame){frame.style.zoom=String(Number(value));frame.style.width=`${100/Number(value)}%`;frame.style.height=`${100/Number(value)}%`;}}
  return structuredClone(library);
}
function frameCommand<T>(tab:string,action:string,value:string|null=null):Promise<T>{
  const view=frames.get(tab);if(!view?.contentWindow)return Promise.reject(new Error("Browser page unavailable"));
  return new Promise((resolve,reject)=>{
    const id=crypto.randomUUID();
    const finish=()=>{clearTimeout(timer);window.removeEventListener("message",receive);};
    const receive=(event:MessageEvent)=>{if(event.source!==view.contentWindow||event.data?.type!=="dl-browser-result"||event.data.id!==id)return;finish();if(event.data.error)reject(new Error(event.data.error));else resolve(event.data.result);};
    const timer=setTimeout(()=>{finish();reject(new Error("Browser operation timed out"));},10000);
    window.addEventListener("message",receive);view.contentWindow!.postMessage({type:"dl-browser-command",action,value,id},"*");
  });
}
export function browserFind(action:BrowserFindAction,text:string,tab:string){return frameCommand<BrowserFindResult>(tab,action,text);}
async function captureMockPage(tab:string){
  const snapshot=await frameCommand<{html:string;width:number;height:number;x:number;y:number}>(tab,"screenshot");
  const doc=new DOMParser().parseFromString(snapshot.html,"text/html");
  doc.querySelectorAll("script,iframe,object,embed,link,base,dl-agent-pointer").forEach(node=>node.remove());
  for(const element of doc.querySelectorAll("*"))for(const attribute of [...element.attributes])if(attribute.name.startsWith("on"))element.removeAttribute(attribute.name);
  // This inert, sanitized fixture copy permits html2canvas load callbacks.
  // WebKit also applies sandbox script restrictions to those callbacks; the
  // interactive page remains in its separate opaque-origin iframe.
  const renderer=document.createElement("iframe");renderer.setAttribute("sandbox","allow-same-origin allow-scripts");renderer.inert=true;renderer.setAttribute("aria-hidden","true");
  Object.assign(renderer.style,{position:"fixed",left:"-20000px",top:"0",width:`${snapshot.width}px`,height:`${snapshot.height}px`,border:"0",pointerEvents:"none"});
  try{
    const loaded=new Promise<void>(resolve=>{renderer.onload=()=>resolve();});renderer.srcdoc="<!doctype html>"+doc.documentElement.outerHTML;document.body.append(renderer);await loaded;
    const canvas=await html2canvas(renderer.contentDocument!.body,{width:snapshot.width,height:snapshot.height,x:snapshot.x,y:snapshot.y,windowWidth:snapshot.width,windowHeight:snapshot.height,scrollX:snapshot.x,scrollY:snapshot.y,scale:devicePixelRatio,logging:false});
    return await new Promise<Blob>((resolve,reject)=>canvas.toBlob(blob=>blob?resolve(blob):reject(new Error("Screenshot unavailable")),"image/png"));
  }finally{renderer.remove();}
}
export async function browserScreenshot(tab:string):Promise<{destination:"clipboard"|"download"}>{
  const mobile=/Android|iPhone|iPad/i.test(navigator.userAgent)||matchMedia("(max-width:767px)").matches;
  const captured=captureMockPage(tab);
  if(mobile){
    const url=URL.createObjectURL(await captured);const link=document.createElement("a");link.href=url;link.download=`DL-page-${Date.now()}.png`;link.click();setTimeout(()=>URL.revokeObjectURL(url),1000);return {destination:"download"};
  }
  await navigator.clipboard.write([new ClipboardItem({"image/png":captured})]);return {destination:"clipboard"};
}
