import type { JsonValue, MessageRecord } from "../src/lib/actor-api";
import { addMessage, addTool, emitEvent, newId, now, setActorStatus, store, touchMessage, type ActorStore } from "./state";

type Action = { name: string; summary: string; arguments: JsonValue; outcome: JsonValue; payload: JsonValue };
type Preview = { actor: ActorStore; input: MessageRecord; generation: number; phase: number; pending?: { call: string; action: Action } };
let preview: Preview | null = null;

function text(state: Preview, channel: "assistant" | "reasoning", content: string) {
  emitEvent(state.actor.summary.id, {type:"generation_started", message_id:state.input.id, generation:++state.generation});
  emitEvent(state.actor.summary.id, {type:"generation_delta", message_id:state.input.id, generation:state.generation, channel, text:content});
}
function start(state: Preview, action: Action) {
  const call = newId();
  emitEvent(state.actor.summary.id, {type:"tool_started",message_id:state.input.id,call_id:call,name:action.name});
  state.pending = {call, action};
}
function finish(state: Preview) {
  const pending = state.pending;
  if (!pending) return;
  const {call, action} = pending;
  const id = newId();
  addTool(state.actor, {id,actor_id:state.actor.summary.id,message_id:state.input.id,call_id:call,name:action.name,summary:action.summary,error:null,started_at_ms:now()-800,finished_at_ms:now(),arguments:action.arguments,model_content:action.summary,outcome:action.outcome,payload:action.payload});
  emitEvent(state.actor.summary.id, {type:"tool_finished",message_id:state.input.id,call_id:call,name:action.name,result_id:id,summary:action.summary,error:null,started_at_ms:now()-800,finished_at_ms:now(),outcome:action.outcome});
  delete state.pending;
}
function read(path: string): Action {
  return {name:"files__read_file",summary:`Read ${path}`,arguments:{path},outcome:{path},payload:{path,content:"export const layout = { preserveReadingPosition: true, overlayComposer: true };\n",start_line:1,end_line:1,total_lines:1}};
}
function command(value: string): Action {
  return {name:"terminal__terminal_exec",summary:value,arguments:{command:value},outcome:{command:value,exit_code:0},payload:{command:value,stdout:["webui/src/components/conversation-content.tsx | 49 +++++---", "webui/src/lib/work-timeline.ts               | 26 ++++++", "webui/src/locales/en-US.json                 |  5 +", "webui/src/locales/zh-CN.json                 |  5 +", "", "106 pass", "0 fail", "Type checking completed.", "Build completed."].join("\n"),stderr:"",exit_code:0}};
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
  actor.detail.config!.permissions.writable_roots = ["~/research"];
  const input = addMessage(actor, {kind:"request",source:{kind:"user"},reply_to:{kind:"user"},content:thinkingOnly?"先想一下如何验证消息区的布局。":"检查消息区的阅读体验，修正布局并验证桌面和手机。",status:"running"});
  touchMessage(input, {started_at_ms:now()-48000});
  emitEvent(actor.summary.id,{type:"message_added",message_id:input.id});
  setActorStatus(actor,"running");
  const state: Preview = {actor,input,generation:0,phase:0};
  preview = state;
  if (thinkingOnly) {
    text(state,"reasoning","先确认消息流的可读区域，再选择覆盖桌面与窄屏的验证方法。\n\n需要同时检查长正文、连续活动和输入框展开后的阅读位置。");
    return;
  }
  text(state,"assistant","我先检查消息流、活动记录和输入框的边界，再核对窄屏下的阅读位置。");
  text(state,"reasoning","先核对页面中可见的阅读区域，再查看对应的布局文件。");
  const actions: Action[] = [
    {name:"browser__browser_open_page",summary:"检查消息区和活动记录的预览状态",arguments:{url:"http://127.0.0.1:5177/"},outcome:{url:"http://127.0.0.1:5177/"},payload:{url:"http://127.0.0.1:5177/",snapshot:"Browser tab: 19, Title: 工作活动 · 分步演示 · 1000px\nURL: http://127.0.0.1:5177/__mock/frame?scene=activity-replay&width=1000\n\nConversation: Iris\nMessage history\nGrouped activity\nComposer\n\nThe conversation remains readable when the composer grows."}},
    read("webui/src/components/conversation-pane.tsx"),
    read("webui/src/components/work-round.tsx"),
    command("rg -n 'padding|gap|overflow' webui/src/index.css"),
    ...["components/work-round.tsx","components/conversation-content.tsx","index.css"].map(edit),
    command("bun x tsc -p tsconfig.json --noEmit"),
    command("bun test tests/visual-conversation.test.tsx"),
  ];
  for (const [index, action] of actions.entries()) {
    if (index > 0) text(state,"reasoning",`继续核对第 ${index+1} 项，已有的观察保留在同一组活动中。`);
    start(state,action); finish(state);
  }
  text(state,"reasoning","这里还需要保留连续工具调用的顺序，展开时才能核对完整记录。\n\n正在核对活动标题与正文的分界，并检查窄屏下是否出现重叠。");
}

// Manual stepping keeps previews and tests deterministic while exercising the
// mounted WebUI through the same incremental SSE facts as a real running Actor.
export function advanceActivityPreview(): boolean {
  const state = preview;
  if (!state || store.actors.get(state.actor.summary.id) !== state.actor || state.phase >= 4) return false;
  switch (++state.phase) {
    case 1: start(state,read("webui/src/index.css")); break;
    case 2:
      finish(state);
      text(state,"reasoning","活动组的标题与展开状态都需要保持稳定。\n\n继续检查工具完成后的标题切换，确保展开中的记录不会被收起。");
      break;
    case 3:
      text(state,"assistant","布局与类型检查已通过。最后再检查活动分组和窄屏换行，确认没有遮挡。");
      start(state,command("bun test && bun run build"));
      break;
    case 4: {
      finish(state);
      const reply = "消息区和活动记录已调整。连续工具调用合并为一个可展开的活动组，思考时只更新标题。\n\n桌面与手机布局、类型检查及构建均已验证。";
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
  const input = addMessage(actor, {kind:"request",source:{kind:"user"},reply_to:{kind:"user"},content:"逐项检查消息区、活动记录和输入框，保留每一步的检查记录。",status:"completed"});
  const state: Preview = {actor,input,generation:0,phase:4};
  emitEvent(actor.summary.id,{type:"message_added",message_id:input.id});
  for (let group = 1; group <= 10; group++) {
    text(state,"assistant",`第 ${group} 组检查：核对消息区在不同内容长度下的布局。继续检查工具记录和输入框的边界，确保阅读位置保持稳定。`);
    for (let item = 1; item <= 8; item++) {
      text(state,"reasoning","继续核对当前文件的布局与活动记录。");
      start(state,read(`webui/src/layout/check-${group}-${item}.tsx`));
      finish(state);
    }
  }
  const reply = "全部 10 组检查已完成，80 条工具记录保留在对应活动组中。";
  text(state,"assistant",reply);
  touchMessage(input,{started_at_ms:now()-120000,finished_at_ms:now(),reply,final_generation:state.generation});
  emitEvent(actor.summary.id,{type:"message_updated",message_id:input.id,status:"completed"});
  setActorStatus(actor,"idle");
}
