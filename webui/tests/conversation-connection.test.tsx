import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { ConversationConnection } from "../src/components/conversation-connection";
import i18n from "../src/lib/i18n";
import type { ConversationConnectionMessage } from "../src/lib/stage-api";
import { coreConnectionMessage, coreConversationReady } from "../src/lib/stage-api";
import { Composer } from "../src/components/composer";
import { TooltipProvider } from "../src/components/ui/tooltip";

const render = (message: ConversationConnectionMessage | null) => renderToStaticMarkup(
  <ConversationConnection message={message} />,
);

test("execution wait exposes an accessible inline status without a modal wait", () => {
  const html = render("conversation_pane.synchronizing");
  expect(html).not.toContain('inert=""');
  expect(html).not.toContain('aria-hidden="true"');
  expect(html.match(/role="status"/g)).toHaveLength(1);
  expect(html).toContain('aria-atomic="true"');
  const recovered = render(null);
  expect(recovered).not.toContain('inert=""');
  expect(recovered).not.toContain('aria-hidden="true"');
  expect(recovered).not.toContain('role="status"');
});
test("core readiness distinguishes local startup, unavailable status and remote recovery", () => {
  const core = {worker_stage_id:null,epoch:1,synchronized:true,ready:false,model_ready:true,state:"recovering"};
  expect(coreConnectionMessage(core)).toBe("conversation_pane.recovering");
  expect(coreConnectionMessage({...core,state:"reconciling"})).toBe("conversation_pane.reconciling");
  expect(coreConnectionMessage({...core,state:"offline"})).toBe("conversation_pane.offline");
  expect(coreConnectionMessage({...core,state:"model_required",model_ready:false})).toBeNull();
  const message = coreConnectionMessage({...core,ready:true,state:"local_recovery"});
  expect(message).toBeNull();
  expect(coreConnectionMessage(undefined)).toBe("conversation_pane.preparing");
  expect(coreConnectionMessage(null)).toBe("conversation_pane.service_unavailable");
  expect(coreConnectionMessage({...core,state:"connecting"})).toBe("conversation_pane.preparing");
  expect(coreConnectionMessage({...core,state:"unexpected"})).toBe("conversation_pane.service_unavailable");
});
test("submissionDisabled leaves the textarea editable and disables the send control", () => {
  const html = renderToStaticMarkup(<TooltipProvider><Composer value="Retained draft" onValueChange={()=>{}} onSubmit={()=>{}}
    attachments={[]} onFiles={()=>{}} onRemoveAttachment={()=>{}} uploading={false} busy={false}
    submissionDisabled canManage={false} models={[]} modelName="" onModelChange={()=>{}} /></TooltipProvider>);
  const textarea = html.match(/<textarea\b[^>]*>/)?.[0];
  const label = i18n.t("composer.send_message");
  const send = [...html.matchAll(/<button\b[^>]*>/g)].map(match => match[0]).find(button => button.includes(`aria-label="${label}"`));
  expect(textarea).toBeDefined();
  expect(send).toBeDefined();
  expect(textarea).not.toContain('disabled=""');
  expect(send).toContain('disabled=""');
});

test("a ready remote Core permits conversation without a local model", () => {
  const core = {worker_stage_id:"remote",epoch:1,synchronized:true,ready:true,model_ready:false,state:"replica"};
  expect(coreConversationReady(core, false)).toBe(true);
  expect(coreConnectionMessage(core)).toBeNull();
  const offline = {...core,ready:false,model_ready:true,state:"offline"};
  expect(coreConversationReady(offline, false)).toBe(true);
  expect(coreConnectionMessage(offline)).toBe("conversation_pane.offline");
  const unconfigured = {...core,ready:false,state:"model_required"};
  expect(coreConversationReady(unconfigured, false)).toBe(false);
  expect(coreConnectionMessage(unconfigured)).toBeNull();
});
