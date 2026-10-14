import {afterEach, expect, test} from "bun:test";
import {renderToStaticMarkup} from "react-dom/server";
import {PublicLoginStatusView} from "../src/components/settings/public-login-status";
import type {PublicLoginStatus} from "../src/lib/stage-api";
import i18n from "../src/lib/i18n";
const status = (state: PublicLoginStatus["state"], error_code: string | null = null): PublicLoginStatus => ({state,error_code,changed_at_ms:1,last_confirmed_at_ms:null,retry_at_ms:null});
afterEach(async()=>{await i18n.changeLanguage("zh-CN");});
test("a saved connection origin is not presented as an online login registration", async()=>{
  await i18n.changeLanguage("zh-CN");
  let html=renderToStaticMarkup(<PublicLoginStatusView status={status("connecting")} />);
  expect(html).toContain("正在连接");expect(html).not.toContain("已连接");
  html=renderToStaticMarkup(<PublicLoginStatusView status={status("reconnecting","public_login_heartbeat_timeout")} />);
  expect(html).toContain("连接已失活");expect(html).toContain("public_login_heartbeat_timeout");expect(html).not.toContain("已连接");
});
test("registration recovery and failed refresh show their actual states in both languages", async()=>{
  await i18n.changeLanguage("en-US");
  expect(renderToStaticMarkup(<PublicLoginStatusView status={status("online")} />)).toContain("Connected");
  expect(renderToStaticMarkup(<PublicLoginStatusView status={null} />)).toContain("Unknown");
  await i18n.changeLanguage("zh-CN");
  expect(renderToStaticMarkup(<PublicLoginStatusView status={status("online")} />)).toContain("已连接");
});
