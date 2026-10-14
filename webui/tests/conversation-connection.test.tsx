import { afterEach, expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { ConversationConnection } from "../src/components/conversation-connection";
import i18n from "../src/lib/i18n";
import type { ConversationConnectionMessage } from "../src/lib/stage-api";
import { connectionBlocks, coreConnectionMessage } from "../src/lib/stage-api";
import { Composer } from "../src/components/composer";
import { TooltipProvider } from "../src/components/ui/tooltip";

afterEach(async () => { await i18n.changeLanguage("zh-CN"); });

const render = (message: ConversationConnectionMessage | null) => renderToStaticMarkup(
  <ConversationConnection message={message}>
    <p>Existing conversation</p><textarea defaultValue="Retained draft" /><button>Send</button>
  </ConversationConnection>,
);

test("connection wait preserves the conversation behind the original ring overlay", async () => {
  await i18n.changeLanguage("zh-CN");
  const html = render("conversation_pane.synchronizing");
  expect(html).toContain('inert=""');
  expect(html).toContain('aria-hidden="true"');
  expect(html).toContain('dl-conversation-connection');
  expect(html).not.toContain('dl-connection-notice');
  expect(html).toContain("Existing conversation");
  expect(html).toContain("Retained draft");
  expect(html.match(/role="status"/g)).toHaveLength(1);
  expect(html).toContain('aria-atomic="true"');
  expect(html).toContain("正在同步对话…");
  const recovered = render(null);
  expect(recovered).not.toContain('inert=""');
  expect(recovered).not.toContain('aria-hidden="true"');
  expect(recovered).not.toContain('role="status"');
  expect(recovered).toContain("Retained draft");
});
test("actual recovery states replace a generic connection message and local execution permits sending", () => {
  const core = {worker_stage_id:null,epoch:1,synchronized:true,ready:false,model_ready:true,state:"recovering"};
  expect(coreConnectionMessage(core)).toBe("conversation_pane.recovering");
  expect(coreConnectionMessage({...core,state:"reconciling"})).toBe("conversation_pane.reconciling");
  expect(coreConnectionMessage({...core,state:"offline"})).toBe("conversation_pane.offline");
  const message = coreConnectionMessage({...core,ready:true,state:"local_recovery"});
  expect(message).toBeNull();
  expect(connectionBlocks(message)).toBe(false);
  expect(connectionBlocks("conversation_pane.reconciling")).toBe(true);
  expect(render("conversation_pane.offline")).toContain("animate-spin");
  expect(render(message)).not.toContain("animate-spin");
});
test("a blocked submission still allows composing and keeps the draft", () => {
  const html = renderToStaticMarkup(<TooltipProvider><Composer value="Retained draft" onValueChange={()=>{}} onSubmit={()=>{}}
    attachments={[]} onFiles={()=>{}} onRemoveAttachment={()=>{}} uploading={false} busy={false} working={false}
    onStop={()=>{}} submissionDisabled canManage={false} mode="ask" onModeChange={()=>{}} models={[]} modelName="" onModelChange={()=>{}} /></TooltipProvider>);
  expect(html).toContain("Retained draft");
  expect(html.match(/<textarea[^>]*>/)?.[0]).not.toContain('disabled=""');
  expect(html.match(/<button[^>]*aria-label="发送消息"[^>]*>/)?.[0]).toContain('disabled=""');
});

test("the current connection stage is localized without fabricated percentages", async () => {
  await i18n.changeLanguage("en-US");
  expect(render("app.connecting")).toContain("Connecting…");
  expect(render("conversation_pane.synchronizing")).toContain("Synchronizing conversation…");
  expect(render("conversation_pane.reconnecting")).toContain("Reconnecting");
  expect(render("conversation_pane.synchronizing")).not.toContain('aria-valuenow');
  await i18n.changeLanguage("zh-CN");
  expect(render("conversation_pane.reconnecting")).toContain("重新连接中");
});
