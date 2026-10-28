import "../src/lib/i18n";
import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { BrowserDownloads } from "../src/components/browser-downloads";
import { ModelDiscovery } from "../src/components/settings/model-discovery";
import { DeviceList } from "../src/components/settings/device-list";
import { pageScrollRoot } from "../src/components/lazy-page";
import { activateScenario } from "../mock/scenarios";
import { handle } from "../mock/routes";

test("paged collections expose scroll boundaries without manual pagination controls", () => {
  const views = [
    <BrowserDownloads library={{history:[],downloads:[],next_offset:24}} onMore={async()=>{}} onAction={()=>{}} />,
    <DeviceList local={null} devices={[]} loading={false} error={null} cursor="next-device" onMore={async()=>{}} />,
    <ModelDiscovery state={{models:Array.from({length:48},(_,index)=>({id:`model-${index}`,thinking_budgets:[]})),loading:false,error:null,retry:()=>{}}} onSelect={()=>{}} onCustom={()=>{}} />,
  ];
  for (const view of views) {
    const html=renderToStaticMarkup(view);
    expect(html).toContain("data-lazy-page");
    expect(html).not.toMatch(/Load more|Show more|More devices/);
  }
  expect(renderToStaticMarkup(<BrowserDownloads library={{history:[],downloads:[],next_offset:null}} onMore={async()=>{}} onAction={()=>{}} />)).not.toContain("data-lazy-page");
});

test("scroll boundaries use the nearest scrolling surface, including popup scroll roots", () => {
  const previous=globalThis.getComputedStyle;
  const outer={parentElement:null,overflowY:"auto"};
  const popup={parentElement:outer,overflowY:"scroll"};
  const content={parentElement:popup,overflowY:"visible"};
  globalThis.getComputedStyle=((node:{overflowY:string})=>({overflowY:node.overflowY})) as typeof getComputedStyle;
  try {
    expect(pageScrollRoot({parentElement:content} as unknown as HTMLElement)).toBe(popup as unknown as HTMLElement);
    expect(pageScrollRoot({parentElement:null} as HTMLElement)).toBeNull();
  } finally {globalThis.getComputedStyle=previous;}
});

test("multi-page app fixtures preserve their cursor after a transient next-page failure", async () => {
  activateScenario("apps-pagination-retry");
  const read=(after="")=>handle(new Request(`http://127.0.0.1/v1/apps?limit=24&after=${after}`));
  const first=await (await read()).json();
  expect(first.items).toHaveLength(24);
  expect((await read(first.next_cursor)).status).toBe(503);
  const second=await (await read(first.next_cursor)).json();
  const last=await (await read(second.next_cursor)).json();
  expect(last.next_cursor).toBeNull();
  expect(new Set([...first.items,...second.items,...last.items].map(item=>item.id)).size).toBe(72);
});
