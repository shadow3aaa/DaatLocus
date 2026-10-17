import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { TooltipProvider } from "../src/components/ui/tooltip";
import { PrimaryTurn } from "../src/components/conversation-turns";
import { MessageQueue } from "../src/components/message-queue";
import { messageDisplayText } from "../src/lib/conversation-import";
import { CODEX_IMPORT_MEDIA_TYPE } from "../src/lib/import-types";
import type { MessageRecord } from "../src/lib/actor-api";

test("import cards and queue previews show source identity without displaying Core instructions", () => {
  const message: MessageRecord = {seq:1,id:"import",actor_id:"core",source:{kind:"user"},reply_to:{kind:"user"},kind:"request",in_reply_to:null,content:"INTERNAL_IMPORT_INSTRUCTIONS",attachments:[{id:"source",name:"My <conversation> 🔵",media_type:CODEX_IMPORT_MEDIA_TYPE,size_bytes:100,uri:"/v1/actors/core/resources/source"}],status:"queued",cancel_requested:false,reply:null,error:null,created_at_ms:1,updated_at_ms:1};
  const chat = {messages:[],tools:[],approvals:[],events:[],generations:{},toolNext:{}} as unknown as Parameters<typeof PrimaryTurn>[0]["chat"];
  const turn = renderToStaticMarkup(<TooltipProvider><PrimaryTurn message={message} chat={chat} access={{kind:"owner",manage:true}} onActor={()=>{}} /></TooltipProvider>);
  const queue = renderToStaticMarkup(<TooltipProvider><MessageQueue messages={[message]} chat={chat} access={{kind:"owner",manage:true}} /></TooltipProvider>);
  for (const html of [turn,queue]) {
    expect(html).toContain("My &lt;conversation&gt; 🔵");
    expect(html).not.toContain("INTERNAL_IMPORT_INSTRUCTIONS");
    expect(html).not.toContain(CODEX_IMPORT_MEDIA_TYPE);
  }
  expect(messageDisplayText(message)).not.toContain("INTERNAL_IMPORT_INSTRUCTIONS");
  expect(messageDisplayText({...message,attachments:[]})).toBe("INTERNAL_IMPORT_INSTRUCTIONS");
});
