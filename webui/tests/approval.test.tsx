import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import i18n from "../src/lib/i18n";
import { WorkTurn } from "../src/components/conversation-turns";
import { messageDisplayText } from "../src/lib/conversation-import";
import { partitionConversationMessages } from "../src/lib/conversation-messages";
import type { ApprovalRecord, MessageRecord } from "../src/lib/actor-api";

test("automatic review records stay out of the work transcript", () => {
  const record: ApprovalRecord={seq:1,id:"private-review",message_id:"input",call:{id:"call",name:"files__read_file",arguments:{path:"PRIVATE_PATH"}},requested:{readable_roots:["PRIVATE_PATH"],writable_roots:[]},decision:"pending",reason:"PRIVATE_REVIEW_REASON",created_at_ms:1,resolved_at_ms:null};
  const message: MessageRecord={seq:1,id:"input",actor_id:"worker",source:{kind:"user"},reply_to:{kind:"user"},kind:"request",in_reply_to:null,content:"Create the report",attachments:[],status:"completed",cancel_requested:false,reply:"Report complete",error:null,created_at_ms:1,updated_at_ms:1};
  for(const decision of ["application_required","pending","approved","denied","cancelled"] as const) {
    const chat={messages:[message],tools:[],approvals:[{...record,decision}],events:[],generations:{},toolNext:{}} as unknown as Parameters<typeof WorkTurn>[0]["chat"];
    const html=renderToStaticMarkup(<WorkTurn message={message} chat={chat} access={{kind:"owner",manage:true}} onActor={()=>{}}/>);
    expect(html).toContain("Report complete");
    for(const value of [record.id,record.reason,"PRIVATE_PATH"])expect(html).not.toContain(value);
  }
});

test("an appeal decision notification is immediate, hidden and never inline guidance", () => {
  // The runtime resolves an appeal with an immutable envelope addressed to the requesting Actor.
  const envelope = JSON.stringify({approval_id:"9f8b204d-5f32-4bfc-9234-d2e3289a1f33",decision:"approved",instruction:"Permission memory has been updated if approved. No effect was executed. Recheck current targets and retry only if still needed; do not replay uncertain effects.",reason:"CORE_APPROVAL_REASON",type:"approval_appeal_decision"});
  const parent: MessageRecord = {seq:1,id:"original",actor_id:"worker",source:{kind:"actor",actor_id:"core"},reply_to:{kind:"actor",actor_id:"core",request_id:"delegation"},kind:"request",in_reply_to:null,content:"Original task",attachments:[],status:"completed",cancel_requested:false,reply:null,error:null,created_at_ms:1,updated_at_ms:1};
  const notice: MessageRecord = {...parent,id:"appeal-decision",kind:"request",in_reply_to:"original",content:envelope,status:"completed",started_at_ms:5,finished_at_ms:6,updated_at_ms:6};
  const chat = {messages:[parent,notice],tools:[],approvals:[],events:[],generations:{},toolNext:{}} as unknown as Parameters<typeof WorkTurn>[0]["chat"];
  const render = (message: MessageRecord, searchQuery?: string) => renderToStaticMarkup(<WorkTurn message={message} chat={chat} access={{kind:"owner",manage:true}} onActor={()=>{}} searchQuery={searchQuery} />);
  // While it waits it is no user-observable queue entry and no transcript turn.
  const queued = {...notice,status:"queued" as const};
  expect(partitionConversationMessages([queued],false)).toEqual({transcript:[]});
  // Once claimed it runs its own turn: the machine input stays hidden, the round remains.
  const turn = render(notice);
  expect(turn).not.toContain("9f8b204d");
  expect(turn).not.toContain("instruction");
  expect(turn).not.toContain(i18n.t("conversation_turns.guidance_applied"));
  expect(turn).toContain('aria-expanded="false"');
  // Search, history previews and copies never expose it either.
  expect(messageDisplayText(notice)).toBe("");
  expect(render(notice,"9f8b204d")).not.toContain("9f8b204d");
  // A user who pastes the same JSON keeps their original text.
  const pasted: MessageRecord = {...parent,id:"pasted",source:{kind:"user"},kind:"request",content:envelope};
  expect(messageDisplayText(pasted)).toBe(envelope);
  expect(render(pasted)).toContain("9f8b204d");
});
