import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { ConversationConnection } from "../src/components/conversation-connection";
import i18n from "../src/lib/i18n";
import type { ConversationConnectionMessage } from "../src/lib/stage-api";
import { connectionBlocks, coreConnectionMessage } from "../src/lib/stage-api";
import { Composer } from "../src/components/composer";
import { TooltipProvider } from "../src/components/ui/tooltip";

const render = (message: ConversationConnectionMessage | null) => renderToStaticMarkup(
  <ConversationConnection message={message}>
    <p>Existing conversation</p><textarea defaultValue="Retained draft" /><button>Send</button>
  </ConversationConnection>,
);

test("connection wait marks background content inaccessible and exposes one status", () => {
  const html = render("conversation_pane.synchronizing");
  expect(html).toContain('inert=""');
  expect(html).toContain('aria-hidden="true"');
  expect(html).toContain("Existing conversation");
  expect(html).toContain("Retained draft");
  expect(html.match(/role="status"/g)).toHaveLength(1);
  expect(html).toContain('aria-atomic="true"');
  const recovered = render(null);
  expect(recovered).not.toContain('inert=""');
  expect(recovered).not.toContain('aria-hidden="true"');
  expect(recovered).not.toContain('role="status"');
  expect(recovered).toContain("Retained draft");
});
test("core readiness selects the blocking reason and local recovery clears the block", () => {
  const core = {worker_stage_id:null,epoch:1,synchronized:true,ready:false,model_ready:true,state:"recovering"};
  expect(coreConnectionMessage(core)).toBe("conversation_pane.recovering");
  expect(coreConnectionMessage({...core,state:"reconciling"})).toBe("conversation_pane.reconciling");
  expect(coreConnectionMessage({...core,state:"offline"})).toBe("conversation_pane.offline");
  const message = coreConnectionMessage({...core,ready:true,state:"local_recovery"});
  expect(message).toBeNull();
  expect(connectionBlocks(message)).toBe(false);
  expect(connectionBlocks("conversation_pane.reconciling")).toBe(true);
});
test("submissionDisabled leaves the textarea editable and disables the send control", () => {
  const html = renderToStaticMarkup(<TooltipProvider><Composer value="Retained draft" onValueChange={()=>{}} onSubmit={()=>{}}
    attachments={[]} onFiles={()=>{}} onRemoveAttachment={()=>{}} uploading={false} busy={false} working={false}
    onStop={()=>{}} submissionDisabled canManage={false} mode="ask" onModeChange={()=>{}} models={[]} modelName="" onModelChange={()=>{}} /></TooltipProvider>);
  const textarea = html.match(/<textarea\b[^>]*>/)?.[0];
  const label = i18n.t("composer.send_message");
  const send = [...html.matchAll(/<button\b[^>]*>/g)].map(match => match[0]).find(button => button.includes(`aria-label="${label}"`));
  expect(textarea).toBeDefined();
  expect(send).toBeDefined();
  expect(textarea).not.toContain('disabled=""');
  expect(send).toContain('disabled=""');
});
