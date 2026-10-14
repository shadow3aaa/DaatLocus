import { afterEach, expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { conversationFailureMessage } from "../src/lib/conversation-errors";
import { PrimaryTurn } from "../src/components/conversation-turns";
import i18n from "../src/lib/i18n";

afterEach(async () => { await i18n.changeLanguage("zh-CN"); });
test("certificate rejection and expired credentials have distinct localized reasons", async () => {
  await i18n.changeLanguage("zh-CN");
  expect(conversationFailureMessage("model_certificate_revoked")).toContain("证书已被吊销");
  expect(conversationFailureMessage("model_certificate_revoked")).not.toContain("凭据");
  expect(conversationFailureMessage("model_authentication_failed")).toContain("凭据无效或已过期");
  await i18n.changeLanguage("en-US");
  expect(conversationFailureMessage("model_certificate_revoked")).toContain("certificate was revoked");
});
test("a synced historical failure retains its user input without rendering provider diagnostics", async () => {
  await i18n.changeLanguage("zh-CN");
  const message = {seq:1,id:"failure",actor_id:"core",source:{kind:"user"},reply_to:{kind:"user"},kind:"request",in_reply_to:null,content:"保留原来的用户消息",attachments:[],status:"failed",cancel_requested:false,reply:null,error:"Model request failed: url=https://private.invalid\nmessage_count=56\nestimated_input_tokens=10059\ninvalid peer certificate: Revoked",created_at_ms:1,updated_at_ms:1} as const;
  const chat = {messages:[],tools:[],approvals:[],events:[],generations:{},toolNext:{}} as unknown as Parameters<typeof PrimaryTurn>[0]["chat"];
  const html = renderToStaticMarkup(<PrimaryTurn message={message as unknown as Parameters<typeof PrimaryTurn>[0]["message"]} chat={chat} access={{kind:"owner",manage:true}} onActor={()=>{}} />);
  expect(html).toContain("保留原来的用户消息");
  expect(html).toContain("这次任务未完成");
  for (const detail of ["private.invalid", "message_count", "estimated_input_tokens", "Revoked"]) expect(html).not.toContain(detail);
  expect(conversationFailureMessage("__proto__")).toContain("这次任务未完成");
});
