import { resetApps, seedAppPages } from "./apps";
import { examplesForScene, toolScenes, toolExampleResult } from "./tool-showcase";
import { resetToolPreview, seedToolExamples } from "./tool-showcase-state";
import { seedCoreMotionPreview } from "./core-motion";
import { seedWorkspacePreview } from "./workspace";
import { resetInstructionLayers } from "./system-prompt";
import { advanceActivityPreview, seedActivityPreview, seedGuidanceActivityPreview, seedOverflowActivityPreview, seedPaginatedActivityPreview, seedProseActivityPreview } from "./activity";
import type { CoreStatus, Device } from "../src/lib/stage-api";
import { resetStore } from "./fixtures";
import { addApproval, addMessage, addTool, configHolder, createActor, emitEvent, newId, nextActorName, now, requestOrigin, ROOT_ACTOR_ID, setActorStatus, STAGE_ID, store, touchMessage, type ActorStore } from "./state";

export const scenarios = [
  {id:"contacts", label:"Contacts · Name and message search", actor:ROOT_ACTOR_ID},
  {id:"contacts-many", label:"Contacts · Infinite scrolling", actor:ROOT_ACTOR_ID},
  {id:"imports", label:"Import · Local conversation selection", actor:ROOT_ACTOR_ID},
  {id:"imports-unsupported", label:"Import · Hidden on unsupported devices", actor:ROOT_ACTOR_ID},
  {id:"import-running", label:"Import · Core is preparing an Actor", actor:ROOT_ACTOR_ID},
  {id:"import-failed", label:"Import · Failure", actor:ROOT_ACTOR_ID},
  {id:"apps", label:"Apps · List and connections", actor:ROOT_ACTOR_ID},
  {id:"apps-pagination", label:"Apps · Infinite scrolling", actor:ROOT_ACTOR_ID},
  {id:"apps-pagination-retry", label:"Apps · Retry a failed page", actor:ROOT_ACTOR_ID},
  {id:"apps-empty", label:"Apps · Empty list", actor:ROOT_ACTOR_ID},
  {id:"apps-loading", label:"Apps · Loading", actor:ROOT_ACTOR_ID},
  {id:"apps-error", label:"Apps · Loading failed", actor:ROOT_ACTOR_ID},
  {id:"conversation", label:"Main conversation", actor:ROOT_ACTOR_ID},
  {id:"workspace", label:"Main conversation · Parallel tasks", actor:ROOT_ACTOR_ID},
  {id:"bubble-spacing", label:"Main conversation · Consecutive bubbles and speakers", actor:ROOT_ACTOR_ID},
  {id:"long-core", label:"Main conversation · Long history and replies", actor:ROOT_ACTOR_ID},
  {id:"core-incoming", label:"Main conversation · New messages while working", actor:ROOT_ACTOR_ID},
  {id:"core-motion", label:"Main conversation · Bubble and status animations", actor:ROOT_ACTOR_ID},
  {id:"empty", label:"Empty conversation", actor:ROOT_ACTOR_ID},
  {id:"work", label:"Work conversation", actor:"research"},
  {id:"actor-messages", label:"Collaborators · Message bubbles", actor:"research"},
  ...toolScenes,
  {id:"activity-replay", label:"Work activity · Step-by-step preview", actor:"research"},
  {id:"activity-overflow", label:"Work activity · Long execution and event catch-up", actor:"research"},
  {id:"activity-guidance", label:"Work activity · New messages during execution", actor:"research"},
  {id:"activity-prose", label:"Work activity · Interleaved text and reasoning", actor:"research"},
  {id:"activity-thinking", label:"Work activity · Consecutive tools during reasoning", actor:"research"},
  {id:"activity-timers", label:"Work activity · Suspended inputs and separate timers", actor:"research"},
  {id:"activity-resumed", label:"Work activity · Resume after an intervening task", actor:"research"},
  {id:"activity-tools", label:"Work activity · Running tools", actor:"research"},
  {id:"activity-complete", label:"Work activity · Interleaved outcomes and text", actor:"research"},
  {id:"activity-pagination", label:"Work activity · Long history with infinite scrolling", actor:"research"},
  {id:"thinking-only", label:"Work activity · No tools called yet", actor:"research"},
  {id:"loading", label:"Loading messages", actor:ROOT_ACTOR_ID},
  {id:"request-error", label:"Read failure and retry", actor:ROOT_ACTOR_ID},
  {id:"running", label:"Execution and tool activity", actor:"research"},
  {id:"approval", label:"Scope request · Core confirmation", actor:"research"},
  {id:"failure", label:"Failure and retry", actor:"research"},
  {id:"queue", label:"New messages while working", actor:"research"},
  {id:"attachments", label:"Image and file attachments", actor:ROOT_ACTOR_ID},
  {id:"long-content", label:"Long history, code, tables and diffs", actor:"research"},
  {id:"diff-overflow", label:"File diffs · Long lines, blank lines and infinite scrolling", actor:"research"},
  {id:"history", label:"History markers and hover previews", actor:"research"},
  {id:"many-approvals", label:"Scope request history pagination", actor:ROOT_ACTOR_ID},
  {id:"connecting", label:"Connecting", actor:ROOT_ACTOR_ID},
  {id:"reconnecting", label:"Event stream reconnection", actor:ROOT_ACTOR_ID},
  {id:"synchronizing", label:"Synchronizing", actor:ROOT_ACTOR_ID},
  {id:"recovering", label:"Recovering", actor:ROOT_ACTOR_ID},
  {id:"reconciling", label:"Merging", actor:ROOT_ACTOR_ID},
  {id:"offline", label:"Offline", actor:ROOT_ACTOR_ID},
  {id:"recovery_blocked", label:"Recovery blocked", actor:ROOT_ACTOR_ID},
  {id:"models-empty", label:"No models", actor:ROOT_ACTOR_ID},
  {id:"devices", label:"Device connection status", actor:ROOT_ACTOR_ID},
  {id:"login", label:"Account sign-in", actor:ROOT_ACTOR_ID},
  {id:"registration", label:"Account registration", actor:ROOT_ACTOR_ID},
] as const;
export type ScenarioId = typeof scenarios[number]["id"];
export const previewState: {id:ScenarioId; authenticated:boolean; registered:boolean; core:CoreStatus; devices:Device[]} = {
  id:"conversation", authenticated:true, registered:true,
  core:{worker_stage_id:STAGE_ID, epoch:1, synchronized:true, ready:true, model_ready:true, state:"replica", config_revision:"1"}, devices:[],
};

function message(actor: ActorStore, content: string, running = false) {
  const record = addMessage(actor,{kind:"request",...requestOrigin(actor),content,status:running?"running":"completed"});
  touchMessage(record,{started_at_ms:now()-65000,finished_at_ms:running?null:now()-3000,reply:running?null:"Review complete. The final message is fully visible above the composer.",final_generation:2});
  emitEvent(actor.summary.id,{type:"message_added",message_id:record.id});
  emitEvent(actor.summary.id,{type:"generation_started",message_id:record.id,generation:1});
  emitEvent(actor.summary.id,{type:"generation_delta",message_id:record.id,generation:1,channel:"assistant",text:"I will check the conversation scroll area, then the layout with the composer expanded."});
  return record;
}

export function activateScenario(id: string, exampleId?: string): boolean {
  if (!scenarios.some(scene=>scene.id===id)) return false;
  const examples = examplesForScene(id);
  if (exampleId && !examples.some(example=>example.id===exampleId)) return false;
  resetStore();
  resetToolPreview();
  resetApps(id === "apps-empty");
  if(id.startsWith("apps-pagination"))seedAppPages(id.endsWith("retry"));
  resetInstructionLayers();
  Object.assign(previewState,{id,authenticated:id!=="login"&&id!=="registration",registered:id!=="registration",devices:[],
    core:{worker_stage_id:STAGE_ID,epoch:1,synchronized:true,ready:true,model_ready:true,state:"replica",config_revision:"1"}});
  const core=store.actors.get(ROOT_ACTOR_ID)!;
  const work=store.actors.get("research")!;
  if(id==="approval"||id==="long-content") {
  }
  const clear=(actor:ActorStore)=>{actor.messages=[];actor.events=[];actor.tools=[];actor.approvals=[];actor.messageSeq=0;actor.eventSeq=0;actor.toolSeq=0;};
  if (examples.length || id === "actor-messages") createActor({id:"atlas",name:"Atlas",parent_id:ROOT_ACTOR_ID,apps:[],model:null,workspace:"~",instructions:""});
  if (examples.length) { clear(work); seedToolExamples(work,exampleId ? examples.filter(example=>example.id===exampleId) : examples); }
  if(id.startsWith("contacts")){
    for(const [index,name] of ["Athena","Atlas","Iris","Hermes"].entries()){
        const actor=createActor({id:name.toLowerCase(),name,parent_id:ROOT_ACTOR_ID,apps:[],model:["mock-large","mock-mini","mock-motion",null][index],workspace:"~",instructions:""});
      const input=addMessage(actor,{source:{kind:"actor",actor_id:ROOT_ACTOR_ID},reply_to:{kind:"actor",actor_id:ROOT_ACTOR_ID,request_id:"contacts-task"},kind:"request",content:["Review Atlas's browser fix and confirm the downloads menu scrolls naturally.","Check scrolling and the narrow layout of the browser downloads menu.","Work with Atlas to review message bubbles on mobile.","Summarize the completed interface checks."][index],status:"completed"});
      touchMessage(input,{reply:["Reviewed Atlas's changes. Downloads and history work correctly.","Atlas finished checking downloads. Scrolling loads more items on mobile too.","Sent the review results to Atlas.","Archived the conclusions of this layout review."][index],started_at_ms:now()-30000,finished_at_ms:now()-1000,final_generation:2});
      if(name==="Hermes")setActorStatus(actor,"archived");
      if(name==="Atlas")setActorStatus(actor,"running");
      if(name==="Iris"){
        const example=examplesForScene("tools-actors").find(item=>item.id==="actor-message")!;
        const result={...toolExampleResult(example,actor.summary.id),message_id:input.id,payload:{message_id:"contacts-sent",sent_message:{conversation_url:"/actors/atlas",content:"Atlas, please recheck scrolling in downloads on mobile."}}};
        addTool(actor,result);
        const record=emitEvent(actor.summary.id,{type:"tool_finished",message_id:input.id,call_id:result.call_id,name:result.name,result_id:result.id,summary:"Message delivered",error:null,outcome:{sent_message:true}});
        if(record)record.created_at_ms=input.started_at_ms!+100;
      }
    }
    if(id==="contacts-many")for(let index=0;index<60;index++){
      const name=nextActorName();createActor({id:name.toLowerCase(),name,parent_id:ROOT_ACTOR_ID,apps:[],model:null,workspace:"~",instructions:""});
    }
  }
  if (id === "actor-messages") {
    clear(work);
    const clock = now();
    const request = addMessage(work,{kind:"request",source:{kind:"actor",actor_id:ROOT_ACTOR_ID},reply_to:{kind:"actor",actor_id:ROOT_ACTOR_ID,request_id:"layout-request"},content:"Review message bubble colors on desktop and mobile.",status:"completed"});
    const bytes = new TextEncoder().encode("# Layout review\n\nMessage content should follow the bubble theme.\n");
    const attachment = {id:"bubble-layout-note",name:"layout-review.md",media_type:"text/markdown",size_bytes:bytes.length,uri:"/v1/actors/research/resources/bubble-layout-note"};
    store.attachments.set(attachment.id,{attachment,bytes,media_type:attachment.media_type});
    request.attachments = [attachment];
    touchMessage(request,{created_at_ms:clock-60000,started_at_ms:clock-59000,finished_at_ms:clock-30000,reply:"Done. My messages use light green bubbles; incoming messages use white bubbles."});
    const followup = addMessage(work,{kind:"request",source:{kind:"actor",actor_id:"atlas"},reply_to:{kind:"actor",actor_id:"atlas",request_id:"followup"},content:"Please preserve wrapping in **Markdown**, code and long paragraphs.\n\nKeep the tool activity presentation unchanged.",status:"completed"});
    touchMessage(followup,{created_at_ms:clock-20000,started_at_ms:clock-19000,finished_at_ms:clock-1000,reply:"Checked ✅\n\nMessages support **Markdown**, `inlineCode` and Emoji.\n\n> Cards, tables, quotes and buttons follow the bubble theme.\n\n```ts\nconst greeting = 'Hello, world 🌍';\nconsole.log(greeting);\n```\n\n| Check | Result |\n| --- | --- |\n| Mobile wrapping | OK |\n| Bubble theme | Consistent |"});
  }
  if (id === "actor-messages") {
    const request = work.messages[0];
    const examples = examplesForScene("tools-actors").filter(example => ["actor-create", "actor-message", "actor-list"].includes(example.id));
    const prose = emitEvent(work.summary.id,{type:"generation_delta",message_id:request.id,generation:1,channel:"assistant",text:"I will review the existing layout, then check mobile sizes."});
    if (prose) prose.created_at_ms = request.started_at_ms! + 500;
    for (const [index, example] of examples.entries()) {
      const result = {...toolExampleResult(example, work.summary.id),message_id:request.id};
      addTool(work,result);
      const started = emitEvent(work.summary.id,{type:"tool_started",message_id:request.id,call_id:result.call_id,name:result.name});
      const finished = emitEvent(work.summary.id,{type:"tool_finished",message_id:request.id,call_id:result.call_id,name:result.name,result_id:result.id,summary:result.summary,error:result.error,outcome:result.outcome});
      if (started) started.created_at_ms = request.started_at_ms! + 1000 + index*2000;
      if (finished) finished.created_at_ms = request.started_at_ms! + 2000 + index*2000;
    }
    request.final_generation = 2;
  }
  if (id === "core-motion") { clear(core); seedCoreMotionPreview(core); }
  if (id === "workspace") { clear(core); clear(work); seedWorkspacePreview(core,work); }
  if (id.startsWith("activity-") || id === "thinking-only") {
    clear(work);
    if (id === "activity-pagination") seedPaginatedActivityPreview(work);
    else if (id === "activity-overflow") seedOverflowActivityPreview(work);
    else if (id === "activity-guidance") seedGuidanceActivityPreview(work);
    else if (id === "activity-prose") seedProseActivityPreview(work);
    else if (id === "activity-resumed") {
      const clock = now();
      const original = message(work,"Review and fix the browser layout.",true);
      touchMessage(original,{created_at_ms:clock-300000,started_at_ms:clock-300000});
      const incoming = message(work,"Also check narrow screens and downloads.");
      touchMessage(incoming,{created_at_ms:clock-180000,started_at_ms:clock-180000,finished_at_ms:clock-60000,reply:"Narrow screens and downloads are checked. Resuming the original layout fix."});
      for (const record of work.events) if ("message_id" in record.event) record.created_at_ms = (record.event.message_id === original.id ? clock-300000 : clock-180000)+1000;
      const resumed = emitEvent(work.summary.id,{type:"generation_delta",message_id:original.id,generation:3,channel:"assistant",text:"Received the additional review. Resuming the browser layout fix."});
      if (resumed) resumed.created_at_ms = clock-30000;
      setActorStatus(work,"running");
    }
    else if (id === "activity-timers") {
      const clock = now();
      for (let index=0;index<3;index++) {
        const input=message(work,["Review the current implementation.","Also review the narrow layout.","Continue addressing the latest feedback."][index],true);
        touchMessage(input,{started_at_ms:clock-(5-index*2)*60000});
      }
      setActorStatus(work,"running");
    }
    else seedActivityPreview(work, id === "thinking-only");
    if (id === "activity-tools") advanceActivityPreview();
    if (id === "activity-complete") while (advanceActivityPreview()) { /* Seed all settled phases. */ }
  }
  if (id.startsWith("import-")) {
    clear(core);
    const record = addMessage(core, {kind:"request",source:{kind:"user"},reply_to:{kind:"user"},content:"Import the attached Codex conversation; prepare an idle Actor without sending work.",status:id==="import-running"?"running":"failed",attachments:[{id:"import-example",name:"Review message scrolling and the composer",media_type:"application/vnd.daat-locus.codex-conversation+json",size_bytes:640,uri:"/v1/actors/core/resources/import-example"}]});
    if(id==="import-failed")touchMessage(record,{error:"Model provider is unavailable",finished_at_ms:now()});
    emitEvent(core.summary.id,{type:"message_added",message_id:record.id});
  }
  if (id==="empty") clear(core);
  if (id === "core-incoming") {
    clear(core);
    message(core, "Please review this interface layout.", true);
    addMessage(core, {kind:"request",source:{kind:"user"},reply_to:{kind:"user"},content:"One more thing: check how it looks on mobile too.",status:"queued"});
    addMessage(core, {kind:"request",source:{kind:"user"},reply_to:{kind:"user"},content:"Preserve the draft being typed.",status:"queued"});
    setActorStatus(core, "running");
  }
  if (id === "bubble-spacing") {
    clear(core);
    const request = message(core, "Send it again so I can review it.");
    touchMessage(request, { reply: "Here is the revised draft again. I will check the requirements and any remaining gaps." });
    const deliver = (reply: string) => {
      const record = addMessage(core, { kind: "delivery", source: {kind: "actor", actor_id: ROOT_ACTOR_ID}, reply_to: {kind: "user"}, content: "", status: "completed" });
      touchMessage(record, {reply, finished_at_ms: now()});
      return record;
    };
    // An internal record between deliveries must not create an empty visual turn.
    deliver("");
    deliver("Checked all 18 pages and four figures, including the fix to Figure 3.\n\nThe draft is ready for review. Please check the open questions on page 17 and whether the performance targets on page 12 match your expectations.");
    const followUp = message(core, "Are those sections required by the template?");
    touchMessage(followUp, {reply: "I will check the source of those two sections and distinguish the instructor's requirements from structure I added."});
    const report = deliver("My notes from the guide support both types of content, but I designed the tables:\n\n- Requirements: ID, author, date and original description.\n- Revisions: changes after review and open questions.\n\nI still need to check the source to see whether the exact columns and styling are required.");
    addTool(core, {id: newId(), actor_id: ROOT_ACTOR_ID, message_id: report.id, call_id: newId(), name: "actors__message", summary: "Check the source", error: null, arguments: {}, model_content: "Assigned to Metis", outcome: {actor_id: "research", name: "Metis"}, payload: null});
    deliver("I will check the source too, to identify what must stay and what can be simplified.");
    addMessage(core, {kind: "request", source: {kind: "user"}, reply_to: {kind: "user"}, content: "Yes, check the source again.", status: "queued"});
    addMessage(core, {kind: "request", source: {kind: "user"}, reply_to: {kind: "user"}, content: "Keep the open questions.", status: "queued"});
  }
  if (["connecting","synchronizing","recovering","reconciling","offline","recovery_blocked"].includes(id)) {
    Object.assign(previewState.core,{state:id,ready:false,synchronized:false});
  }
  if (id==="models-empty") {configHolder.current={providers:[],models:[],main_model:null,approval_model:null};Object.assign(previewState.core,{state:"model_required",model_ready:false});}
  if (["running","approval","failure","queue"].includes(id)) {
    const active=message(work,"Check conversation scrolling and preserve the composer draft.",id!=="failure");
    const call=newId();
    const toolName=id==="approval"?"files__edit_file":"files__read_file";
    emitEvent(work.summary.id,{type:"tool_started",message_id:active.id,call_id:call,name:toolName});
    if(id!=="failure") setActorStatus(work,"running");
    if(id==="approval") {
      const approval=addApproval(work,{message_id:active.id,call:{id:call,name:"files__edit_file",arguments:{path:"/reports/layout.tsx",old_text:"padding-bottom: 24px;",new_text:"padding-bottom: 32px;"}},requested:{readable_roots:[],writable_roots:["/reports"]}});
      approval.decision="denied";
      approval.reason="The requested directory is outside the current sandbox scope.";
      approval.resolved_at_ms=now();
      emitEvent(work.summary.id,{type:"approval_resolved",message_id:active.id,approval_id:approval.id,decision:"denied",reason:approval.reason});
      const reply="Additional directory write access is needed. Submitted the request to Core.";
      touchMessage(active,{status:"completed",reply,finished_at_ms:now(),final_generation:2});
      emitEvent(work.summary.id,{type:"message_updated",message_id:active.id,status:"completed"});
      setActorStatus(work,"idle");
      const request=message(core,"Generate a layout report." );
      touchMessage(request,{reply:"The report needs write access to /reports. Do you approve?"});
    }
    if(id==="failure") touchMessage(active,{status:"failed",reply:null,error:"provider_unavailable"});
    if(id==="queue") for(const content of ["Also check the layout at 320px.","Include long code blocks and tables in the review.","Remember to preserve the composer draft."]){const queued=addMessage(work,{kind:"request",...requestOrigin(work),content,status:"queued"});emitEvent(work.summary.id,{type:"message_added",message_id:queued.id});}
  }
  if(id==="long-core") {
    for(let i=0;i<36;i++) {
      const record=message(core,`Review record ${i+1}: Continue reviewing the message layout on desktop and mobile.`);
      touchMessage(record,{reply:`Stage ${i+1} checked. The conversation and composer remain aligned.`});
    }
    const record=message(core,"Summarize the review conclusions.");
    touchMessage(record,{reply:"Interface review complete. Here are the results.\n\n"+Array.from({length:6},(_,i)=>`### ${i+1}. ${["Window and boundaries","Message reading","Long content","Composer","Navigation","Narrow layout"][i]}

Messages scroll naturally in the main area, beneath the floating header and composer. Long paragraphs wrap to the window width, and the last paragraph remains fully readable.

- Checked desktop and mobile layouts.
- Preserved the reading position and unsent drafts.`).join("\n\n")+"\n\nAll checks are complete."});
  }
  if(id==="history") {
    clear(work);
    for(let i=0;i<49;i++) {
      const record=message(work,`Stage ${i+1}: ${["Check the reading position while scrolling.","Review spacing with the composer expanded.","Check narrow-screen wrapping and the back button."][i%3]}`);
      touchMessage(record,{reply:i%3===0 ? "This layout is checked.\n\n- Messages preserve the reading position.\n- The end stays readable with the composer expanded.\n- Mobile and desktop use the same message content." : "This section is checked. Reading continues smoothly when the window changes."});
    }
    const record=message(work,"Summarize this round of interface checks.");
    touchMessage(record,{reply:"This layout review is complete.\n\n- History preserves the reading position.\n- The end remains fully visible when the composer expands.\n- Narrow screens use a full-screen conversation with a back button.\n\nAlso checked wrapping in long paragraphs, lists and code."});
    // The task started outside the newest page. Opening its card should still
    // enter at the newest edge; only an explicit search should fetch that input.
    const task = core.tools.find(tool => tool.result.name === "actors__message");
    if (task) task.result.outcome = {actor_id:"research",name:"Metis",message_id:work.messages[0].id};
  }
  if(id==="long-content") {
    for(let i=0;i<44;i++) message(work,`Stage ${i+1}: The conversation, header and composer stay stable when resizing.`);
    const record=message(work,"Check scrolling in long code, tables and file diffs." );
    touchMessage(record,{reply:"Layout review results:\n\n```typescript\n"+Array.from({length:32},(_,i)=>`const viewport${i+1} = measureConversationLayout({ keepReadingPosition: true, reserveComposerSpace: true, preserveAttachmentDraft: true });`).join("\n")+"\n```\n\n| Area | Desktop | Phone | Reading position | Expanded composer | Last message |\n| --- | --- | --- | --- | --- | --- |\n"+Array.from({length:16},(_,i)=>`| Check ${i+1} | OK | OK | Preserved | Checked | Fully visible |`).join("\n")});
    const toolId=newId(), callId=newId();
    addTool(work,{id:toolId,actor_id:work.summary.id,message_id:record.id,call_id:callId,name:"files__edit_file",summary:"Adjust bottom spacing in the conversation",error:null,arguments:{path:"webui/src/index.css"},model_content:"Updated conversation spacing.",outcome:{path:"webui/src/index.css"},payload:{path:"webui/src/index.css",change:{added_lines:24,removed_lines:24,lines:Array.from({length:48},(_,i)=>({kind:i%2?"add":"remove",old_line:i%2?null:i+1,new_line:i%2?i+1:null,text:i%2?"padding-bottom: calc(var(--dl-footer-space) + 24px);":"padding-bottom: 24px;"}))}}});
    emitEvent(work.summary.id,{type:"tool_finished",message_id:record.id,call_id:callId,name:"files__edit_file",result_id:toolId,summary:"Adjust bottom spacing in the conversation",error:null});
  }
  if(id==="diff-overflow") {
    clear(work);
    const record=message(work,"Review long Markdown lines and multi-file diffs." );
    touchMessage(record,{reply:"Updated the changelog and related files."});
    const path="C:\\Users\\Example\\project\\docs\\LONG_ALIGNMENT_CHANGELOG.local.md";
    for (let version=0;version<7;version++) {
      const filename=version<2?path:`src/components/layout-${version}.tsx`;
      const lines=Array.from({length:version===0?185:12},(_,index)=>{
        const kind=index%6===2?"remove":index%6===3||index%6===0?"add":"context";
        const text=index%6===1?"":index===0?"# Long Markdown verification — "+"Preserve full-row backgrounds, line numbers and original spacing. ".repeat(32):index%6===2?"const result = previousLayout({ padding: 24, selected: false });":index%6===3?"const result = currentLayout({ padding: 32, selected: true });":`Line ${index+1}: preserve source whitespace and <literal> text.\t${"Long source content without wrapping. ".repeat(index%8===4?25:1)}`;
        return {kind,text,old_line:kind==="add"?null:99000+index,new_line:kind==="remove"?null:99000+index};
      });
      const added=lines.filter(line=>line.kind==="add").length, removed=lines.filter(line=>line.kind==="remove").length;
      const toolId=newId(),callId=newId();
      addTool(work,{id:toolId,actor_id:work.summary.id,message_id:record.id,call_id:callId,name:"files__edit_file",summary:"Update file",error:null,arguments:{path:filename},model_content:"File updated.",outcome:{path:filename,added_lines:added,removed_lines:removed},payload:{path:filename,change:{added_lines:added,removed_lines:removed,lines}}});
      emitEvent(work.summary.id,{type:"tool_finished",message_id:record.id,call_id:callId,name:"files__edit_file",result_id:toolId,summary:"Update file",error:null});
    }
  }
  if(id==="attachments") {
    const bytes=new TextEncoder().encode("# Layout review\n\nThe conversation fills the container, with a floating composer at the bottom.\n");
    const attachment={id:"layout-note",name:"layout-review.md",media_type:"text/markdown",size_bytes:bytes.length,uri:`/v1/actors/${ROOT_ACTOR_ID}/resources/layout-note`};
    store.attachments.set(attachment.id,{attachment,bytes,media_type:attachment.media_type});
    const imageBytes=new TextEncoder().encode('<svg xmlns="http://www.w3.org/2000/svg" width="640" height="360" viewBox="0 0 640 360"><rect width="640" height="360" rx="24" fill="#eff2f6"/><rect x="32" y="32" width="576" height="296" rx="20" fill="white"/><rect x="340" y="70" width="230" height="48" rx="24" fill="#3566f0"/><rect x="70" y="144" width="370" height="88" rx="22" fill="#f0f0f0"/><rect x="70" y="264" width="500" height="40" rx="20" fill="#f7f7f7"/></svg>');
    const image={id:"layout-image",name:"conversation-layout.svg",media_type:"image/svg+xml",size_bytes:imageBytes.length,uri:`/v1/actors/${ROOT_ACTOR_ID}/resources/layout-image`};
    store.attachments.set(image.id,{attachment:image,bytes:imageBytes,media_type:image.media_type});
    const record=message(core,"Review these layout notes." );record.attachments=[image,attachment];
  }
  if(id==="many-approvals") for(let i=0;i<64;i++) {const record=message(work,`Check ${i+1}` ,true);addApproval(work,{message_id:record.id,call:{id:newId(),name:"files__read_file",arguments:{path:`webui/src/check-${i+1}.tsx`}},requested:{readable_roots:["webui/src"],writable_roots:[]}});}
  if(id==="devices") previewState.devices=[
    {stage_id:"laptop",label:"Laptop",connection:{status:"online",route:"lan",generation:1,retry_at_ms:0}},
    {stage_id:"phone",label:"Phone",connection:{status:"connecting",route:null,generation:2,retry_at_ms:0}},
    {stage_id:"desktop",label:"Desktop",error_code:"connection_failed",connection:{status:"offline",route:null,generation:1,retry_at_ms:0}},
  ];
  return true;
}
