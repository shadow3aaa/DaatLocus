import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { conversationFailureMessage } from "../src/lib/conversation-errors";
import { PrimaryTurn } from "../src/components/conversation-turns";
import i18n from "../src/lib/i18n";

test("billing failure uses its safe localized status", () => {
  expect(conversationFailureMessage("model_billing_required")).toBe(i18n.t("conversation_turns.model_billing_required"));
});

test("historical failures render original input and omit private provider diagnostics", () => {
  const message = {seq:1,id:"failure",actor_id:"core",source:{kind:"user"},reply_to:{kind:"user"},kind:"request",in_reply_to:null,content:"保留原来的用户消息",attachments:[],status:"failed",cancel_requested:false,reply:null,error:"Model request failed: url=https://private.invalid\nmessage_count=56\nestimated_input_tokens=10059\ninvalid peer certificate: Revoked",created_at_ms:1,updated_at_ms:1} as const;
  const chat = {messages:[],tools:[],approvals:[],events:[],generations:{},toolNext:{}} as unknown as Parameters<typeof PrimaryTurn>[0]["chat"];
  const html = renderToStaticMarkup(<PrimaryTurn message={message as unknown as Parameters<typeof PrimaryTurn>[0]["message"]} chat={chat} access={{kind:"owner",manage:true}} onActor={()=>{}} />);
  expect(html).toContain("保留原来的用户消息");
  for (const detail of ["private.invalid", "message_count", "estimated_input_tokens", "Revoked"]) expect(html).not.toContain(detail);
  expect(conversationFailureMessage("__proto__")).toBe(i18n.t("conversation_turns.this_task_is_incomplete_send_a_message_to_continue"));
});
