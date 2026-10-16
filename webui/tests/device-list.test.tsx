import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { DeviceList } from "../src/components/settings/device-list";
import i18n from "../src/lib/i18n";
import type { Device } from "../src/lib/stage-api";

const device = (id: string, label: string, status: string): Device => ({stage_id:id,label,connection:{status,route:null,generation:1,retry_at_ms:0}});
const local = {id:"local",name:"My computer"};

test("the local device is rendered once alongside distinct peers", ()=>{
  const html=renderToStaticMarkup(<DeviceList local={local} devices={[device("local","My computer","online"),device("a","Laptop","online"),device("b","Home PC","backoff"),device("c","Tablet","unreachable")]} loading={false} error={null} hasMore onMore={()=>{}} />);
  for(const text of ["Laptop","Home PC","Tablet"])expect(html).toContain(text);
  expect(html.match(/My computer/g)?.length).toBe(1);
});

test("a refresh error renders cached peer status as unknown", ()=>{
  const html=renderToStaticMarkup(<DeviceList local={local} devices={[device("a","Laptop","online")]} loading={false} error="Connection failed" hasMore={false} onMore={()=>{}} />);
  expect(html).toContain("Laptop");
  expect(html).toContain(i18n.t("account_editor.status_unknown"));
  expect(html).not.toContain(`>${i18n.t("account_editor.online")}<`);
});
