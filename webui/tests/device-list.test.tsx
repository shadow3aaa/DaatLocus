import { afterEach, expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { DeviceList } from "../src/components/settings/device-list";
import i18n from "../src/lib/i18n";
import type { Device } from "../src/lib/stage-api";

const device = (id: string, label: string, status: string): Device => ({stage_id:id,label,connection:{status,route:null,generation:1,retry_at_ms:0}});
const local = {id:"local",name:"My computer"};
afterEach(async()=>{await i18n.changeLanguage("zh-CN");});

test("the current device remains visible when there are no other devices", async()=>{
  await i18n.changeLanguage("zh-CN");
  const html=renderToStaticMarkup(<DeviceList local={local} devices={[]} loading={false} error={null} hasMore={false} onMore={()=>{}} />);
  expect(html).toContain("My computer");expect(html).toContain("此设备");expect(html).toContain("设备");
});

test("device names and actual connection states are visible without enrollment controls", async()=>{
  await i18n.changeLanguage("en-US");
  const html=renderToStaticMarkup(<DeviceList local={local} devices={[device("local","My computer","online"),device("a","Laptop","online"),device("b","Home PC","backoff"),device("c","Tablet","unreachable")]} loading={false} error={null} hasMore onMore={()=>{}} />);
  for(const text of ["Laptop","Home PC","Tablet","Online","Offline","More devices"])expect(html).toContain(text);
  expect(html.match(/My computer/g)?.length).toBe(1);
  expect(html).not.toContain("Reconnecting");expect(html).not.toContain("Connect device");expect(html).not.toContain("Pair");
});

test("failed refresh does not present stale peer states as currently online", async()=>{
  await i18n.changeLanguage("en-US");
  const html=renderToStaticMarkup(<DeviceList local={local} devices={[device("a","Laptop","online")]} loading={false} error="Connection failed" hasMore={false} onMore={()=>{}} />);
  expect(html).toContain("Laptop");expect(html).toContain("Unknown");expect(html).not.toContain(">Online<");
});

test("missing metadata is clearly unnamed and connection failures use localized safe reasons", async()=>{
  await i18n.changeLanguage("zh-CN");
  const peer = {...device("700c040e-4fda", "", "unreachable"), error_code:"device_connection_timeout"};
  const html=renderToStaticMarkup(<DeviceList local={local} devices={[peer]} loading={false} error={null} hasMore={false} onMore={()=>{}} />);
  expect(html).toContain("未命名设备 (700c040e)");
  expect(html).toContain("设备连接超时");
  expect(html).not.toContain("device_connection_timeout");
});
