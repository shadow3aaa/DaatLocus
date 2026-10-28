import "../src/lib/i18n";
import { expect,test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { HighlightedText } from "../src/components/highlighted-text";
import { NavigationRail } from "../src/components/app-navigation";
import { TooltipProvider } from "../src/components/ui/tooltip";
import { contactExcerpt } from "../src/lib/contacts";
import { activateScenario } from "../mock/scenarios";
import { addMessage,store } from "../mock/state";
import { handle } from "../mock/routes";

test("contacts are a sidebar page, with no selected Home entry",()=>{
  const html=renderToStaticMarkup(<TooltipProvider><NavigationRail access={{kind:"owner",manage:true}} settings={false} apps={false} contacts onContacts={()=>{}} onApps={()=>{}} onHome={()=>{}} onSettings={()=>{}} onLogout={()=>{}}/></TooltipProvider>);
  expect(html).toMatch(/aria-label="Contacts"[^>]*aria-current="page"/);
  expect(html).not.toMatch(/aria-label="Home"[^>]*aria-current="page"/);
});

test("matching text is safely highlighted and excerpts retain the match, including Chinese",()=>{
  const html=renderToStaticMarkup(<HighlightedText text={'<script>Atlas</script> ATLAS'} query="atlas"/>);
  expect(html.match(/<mark>/g)).toHaveLength(2);expect(html).toContain("&lt;script&gt;");expect(html).not.toContain("<script>");
  const excerpt=contactExcerpt("旧内容".repeat(80)+"手机上的浏览器滚动"+"结尾".repeat(80),"浏览器");
  expect(excerpt).toContain("浏览器");expect(excerpt.startsWith("…")).toBe(true);expect(excerpt.endsWith("…")).toBe(true);
});

test("contacts search actor names and actual bubbles, including sent-message references",async()=>{
  activateScenario("contacts");
  const read=(q:string)=>handle(new Request(`http://127.0.0.1/v1/contacts?q=${encodeURIComponent(q)}`));
  const page=await (await read("aTlAs")).json();
  expect(page.items.map((item:{actor:{id:string}})=>item.actor.id)).toEqual(["athena","atlas","iris"]);
  const iris=page.items.find((item:{actor:{id:string}})=>item.actor.id==="iris");
  expect(iris.messages).toEqual(expect.arrayContaining([expect.objectContaining({kind:"sent",result_id:"showcase-result-actor-message"})]));
  const core=store.actors.get("core")!;
  addMessage(core,{kind:"request",source:{kind:"actor",actor_id:"iris"},reply_to:{kind:"user"},content:"PRIVATE_COORDINATION",status:"completed"});
  expect((await (await read("PRIVATE_COORDINATION")).json()).items).toEqual([]);
  expect((await (await read("no matching text")).json()).items).toEqual([]);
});

test("contact pages continue from an opaque cursor without duplicating actors",async()=>{
  activateScenario("contacts-many");let after:string|null=null;const seen=new Set<string>();
  do {
    const page=await (await handle(new Request(`http://127.0.0.1/v1/contacts?limit=24${after?`&after=${after}`:""}`))).json();
    expect(page.items.length).toBeLessThanOrEqual(24);
    for(const item of page.items){expect(seen.has(item.actor.id)).toBe(false);seen.add(item.actor.id);}
    after=page.next_cursor;
  }while(after);
  expect(seen.size).toBe(store.actors.size);
});
