import type { JsonValue, MessageRecord } from "../src/lib/actor-api";
import { addMessage, addTool, emitEvent, newId, now, requestOrigin, setActorStatus, store, touchMessage, type ActorStore } from "./state";

type Action = { name: string; summary: string; arguments: JsonValue; start?: JsonValue; outcome: JsonValue; payload: JsonValue };
type Preview = { actor: ActorStore; input: MessageRecord; generation: number; phase: number; overflow?: boolean; pending?: { call: string; action: Action } };
let preview: Preview | null = null;

function text(state: Preview, channel: "assistant" | "reasoning", content: string) {
  emitEvent(state.actor.summary.id, {type:"generation_started", message_id:state.input.id, generation:++state.generation});
  emitEvent(state.actor.summary.id, {type:"generation_delta", message_id:state.input.id, generation:state.generation, channel, text:content});
}
function start(state: Preview, action: Action) {
  const call = newId();
  emitEvent(state.actor.summary.id, {type:"tool_started",message_id:state.input.id,call_id:call,name:action.name,outcome:action.start});
  state.pending = {call, action};
}
function finish(state: Preview) {
  const pending = state.pending;
  if (!pending) return;
  const {call, action} = pending;
  const id = newId();
  addTool(state.actor, {id,actor_id:state.actor.summary.id,message_id:state.input.id,call_id:call,name:action.name,summary:action.summary,error:null,arguments:action.arguments,model_content:action.summary,outcome:action.outcome,payload:action.payload});
  emitEvent(state.actor.summary.id, {type:"tool_finished",message_id:state.input.id,call_id:call,name:action.name,result_id:id,summary:action.summary,error:null,outcome:action.outcome});
  delete state.pending;
}
function read(path: string): Action {
  return {name:"files__read_file",summary:`Read ${path}`,arguments:{path},outcome:{path},payload:{path,content:"export const layout = { preserveReadingPosition: true, overlayComposer: true };\n",start_line:1,end_line:1,total_lines:1}};
}
function command(value: string): Action {
  return {name:"terminal__terminal_exec",summary:value,arguments:{command:value},start:{command:value},outcome:{command:value,exit_code:0},payload:{command:value,stdout:["webui/src/components/conversation-content.tsx | 49 +++++---", "webui/src/lib/work-timeline.ts               | 26 ++++++", "webui/src/locales/en-US.json                 |  5 +", "webui/src/locales/zh-CN.json                 |  5 +", "", "106 pass", "0 fail", "Type checking completed.", "Build completed."].join("\n"),stderr:"",exit_code:0}};
}
function edit(file: string): Action {
  const path = `webui/src/${file}`;
  const lines = [
    {kind:"context",old_line:30,new_line:30,text:"export function messageLayout() {"},
    {kind:"context",old_line:31,new_line:31,text:"  // Keep consecutive activity together."},
    {kind:"remove",old_line:32,new_line:null,text:"  const spacing = { toolGap: 24, preserveReadingPosition: true };"},
    {kind:"add",old_line:null,new_line:32,text:"  const spacing = { toolGap: 4, preserveReadingPosition: true };"},
    {kind:"context",old_line:33,new_line:33,text:"  return spacing;"},
    {kind:"context",old_line:34,new_line:34,text:"}"},
  ];
  return {name:"files__edit_file",summary:`Updated ${file}`,arguments:{path},outcome:{path,added_lines:1,removed_lines:1},payload:{path,change:{added_lines:1,removed_lines:1,lines}}};
}

export function seedActivityPreview(actor: ActorStore, thinkingOnly = false) {
  actor.detail.config!.name = "Iris";
  actor.summary.name = "Iris";
  const input = addMessage(actor, {kind:"request",...requestOrigin(actor),content:thinkingOnly?"Plan how to verify the conversation layout.":"Review the conversation layout, fix any issues, and verify desktop and mobile.",status:"running"});
  touchMessage(input, {started_at_ms:now()-48000});
  emitEvent(actor.summary.id,{type:"message_added",message_id:input.id});
  setActorStatus(actor,"running");
  const state: Preview = {actor,input,generation:0,phase:0};
  preview = state;
  if (thinkingOnly) {
    text(state,"reasoning","First identify the visible conversation area, then choose checks for desktop and narrow screens.\n\nCover long messages, consecutive activities, and the reading position when the composer expands.");
    return;
  }
  text(state,"assistant","I will check the conversation, activity and composer boundaries, then verify the reading position on narrow screens.");
  text(state,"reasoning","Check the visible reading area, then inspect the relevant layout files.");
  const actions: Action[] = [
    {name:"web__run",summary:"Review the conversation and activity preview",arguments:{open:[{ref_id:"http://127.0.0.1:5177/",offset:null,source:"rendered",view:null,refresh:false}],click:null,find:null,response_length:"medium"},outcome:{url:"http://127.0.0.1:5177/"},payload:{results:[{operation:"open",result:{ref_id:"web:preview",title:"Work activity · Step-by-step preview",url:"http://127.0.0.1:5177/",offset:0,next_offset:null,content:"Conversation: Iris\nMessage history\nGrouped activity\nComposer\n\nThe conversation remains readable when the composer grows."}}]}},
    read("webui/src/components/conversation-pane.tsx"),
    read("webui/src/components/work-activity.tsx"),
    command("rg -n 'padding|gap|overflow' webui/src/index.css"),
    ...["components/work-activity.tsx","components/conversation-content.tsx","index.css"].map(edit),
    command("bun x tsc -p tsconfig.json --noEmit"),
    command("bun test tests/visual-conversation.test.tsx"),
  ];
  for (const [index, action] of actions.entries()) {
    if (index > 0) text(state,"reasoning",`Continue reviewing item ${index+1}; keep the existing observations in the same activity group.`);
    start(state,action); finish(state);
  }
  text(state,"reasoning","Consecutive tool calls must retain their order so the full record can be checked when expanded.\n\nChecking the boundary between activity headings and prose, including overlaps on narrow screens.");
}

export function seedOverflowActivityPreview(actor: ActorStore) {
  seedActivityPreview(actor);
  preview!.overflow = true;
}

export function seedProseActivityPreview(actor: ActorStore) {
  seedActivityPreview(actor);
  const state = preview!;
  const chunks: ["assistant" | "reasoning", string][] = [
    ["assistant", "QQ"],
    ["reasoning", "Checking the device conversation."],
    ["assistant", " has a \"My phone\" device-transfer conversation (with a previous APK transfer). **Now"],
    ["reasoning", "Checking build progress."],
    ["assistant", " let me wait** on the build.\n\nThe message body"],
    ["reasoning", "Checking the paragraph boundary."],
    ["assistant", " should remain continuous, preserving its paragraphs and `code` without splitting at reasoning updates."],
  ];
  emitEvent(actor.summary.id,{type:"generation_started",message_id:state.input.id,generation:++state.generation});
  for (const [channel, content] of chunks) emitEvent(actor.summary.id,{type:"generation_delta",message_id:state.input.id,generation:state.generation,channel,text:content});
  start(state,command("bun run build"));
}

export function seedGuidanceActivityPreview(actor: ActorStore) {
  seedActivityPreview(actor);
  const state = preview!;
  const guide = addMessage(actor,{kind:"request",...requestOrigin(actor),in_reply_to:state.input.id,content:"No screenshots needed; report the results directly.",status:"completed"});
  emitEvent(actor.summary.id,{type:"message_added",message_id:guide.id});
  emitEvent(actor.summary.id,{type:"message_updated",message_id:guide.id,status:"completed"});
  text(state,"assistant","Understood. I will verify and report the results without preparing screenshots.");
  start(state,read("webui/src/layout/result.tsx")); finish(state);
  start(state,read("webui/src/layout/summary.tsx"));
  const beginning = now()-60000;
  actor.events.forEach((record,index) => { record.created_at_ms = beginning+index*100; });
  const sent = actor.events.find(record => record.event.type === "message_added" && record.event.message_id === guide.id)!;
  touchMessage(guide,{created_at_ms:sent.created_at_ms,finished_at_ms:sent.created_at_ms+100});
  touchMessage(state.input,{created_at_ms:beginning-100,started_at_ms:beginning});
}

// Manual stepping keeps previews and tests deterministic while exercising the
// mounted WebUI through the same incremental SSE facts as a real running Actor.
export function advanceActivityPreview(): boolean {
  const state = preview;
  if (!state || store.actors.get(state.actor.summary.id) !== state.actor || state.phase >= 4) return false;
  if (state.overflow && state.phase < 3) {
    ++state.phase;
    finish(state);
    text(state,"assistant",`Stage ${state.phase + 1}: keep earlier operations in place and continue reviewing new files.`);
    start(state,read(`webui/src/layout/phase-${state.phase}.tsx`)); finish(state);
    text(state,"reasoning","Starting the next check.");
    // A burst exceeds the bounded conversation stream window. The activity view
    // must retain its prefix or backfill the gap, never append summary leftovers.
    for (let index = 0; index < 400; index++) emitEvent(state.actor.summary.id,{type:"generation_delta",message_id:state.input.id,generation:state.generation,channel:"reasoning",text:`

Reviewing stage ${state.phase + 1} in detail.`});
    start(state,read(`webui/src/layout/phase-${state.phase}-pending.tsx`));
    return true;
  }
  switch (++state.phase) {
    case 1: start(state,read("webui/src/index.css")); break;
    case 2:
      finish(state);
      text(state,"reasoning","Activity headings and expansion state should remain stable.\n\nChecking heading changes after tools finish, without collapsing open records.");
      break;
    case 3:
      text(state,"assistant","Layout and type checks passed. Finishing with activity grouping and narrow-screen wrapping to check for overlaps.");
      start(state,command("bun test && bun run build"));
      break;
    case 4: {
      finish(state);
      const reply = "Updated the conversation and activity layout. Consecutive tools form one expandable group; reasoning updates only its heading.\n\nVerified desktop and mobile layouts, type checks and the build.";
      text(state,"assistant",reply);
      touchMessage(state.input,{status:"completed",reply,finished_at_ms:now(),final_generation:state.generation});
      emitEvent(state.actor.summary.id,{type:"message_updated",message_id:state.input.id,status:"completed"});
      setActorStatus(state.actor,"idle");
      break;
    }
  }
  return true;
}

export function seedPaginatedActivityPreview(actor: ActorStore) {
  actor.detail.config!.name = "Iris";
  actor.summary.name = "Iris";
  const input = addMessage(actor, {kind:"request",...requestOrigin(actor),content:"Review the conversation, activity and composer one at a time, retaining each check.",status:"completed"});
  const state: Preview = {actor,input,generation:0,phase:4};
  emitEvent(actor.summary.id,{type:"message_added",message_id:input.id});
  for (let group = 1; group <= 10; group++) {
    text(state,"assistant",`Stage ${group}: review the conversation with different message lengths, then check tool records and composer boundaries while preserving the reading position.`);
    for (let item = 1; item <= 8; item++) {
      text(state,"reasoning","Continue checking this file's layout and activity records.");
      start(state,read(`webui/src/layout/check-${group}-${item}.tsx`));
      finish(state);
    }
  }
  const reply = "All 10 groups are complete. The 80 tool records remain in their activity groups.";
  text(state,"assistant",reply);
  touchMessage(input,{started_at_ms:now()-120000,finished_at_ms:now(),reply,final_generation:state.generation});
  emitEvent(actor.summary.id,{type:"message_updated",message_id:input.id,status:"completed"});
  setActorStatus(actor,"idle");
}
