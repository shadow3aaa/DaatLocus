import type { ActorEvent, MessageRecord, ToolResult } from "../src/lib/actor-api";
import { addMessage, addTool, emitEvent, now, ROOT_ACTOR_ID, setActorStatus, store, touchMessage, type ActorStore } from "./state";
import { toolExamples, toolExampleResult, toolPreviewResource as resource, type ToolExample } from "./tool-showcase";

let failures = new Set<string>();
const liveResults=new Map<string,ToolResult>();
const liveTimers=new Set<ReturnType<typeof setInterval>>();
export function liveToolResult(id:string) {return liveResults.get(id);}
export function resetToolPreview() { failures = new Set(); for(const timer of liveTimers)clearInterval(timer);liveTimers.clear();liveResults.clear(); }
function seedLiveTerminal(actor:ActorStore,message:MessageRecord,result:ToolResult) {
  liveResults.set(result.id,result);
  emitEvent(actor.summary.id,{type:"tool_updated",message_id:message.id,call_id:result.call_id,result_id:result.id});
  let step=0;
  const timer=setInterval(()=>{
    const complete=++step>=14;
    result.payload={session:{command:"bun test",status:complete?"exited":"running",exit_code:complete?0:null},running:!complete,captured_output:`Starting test suite…\n${Array.from({length:step},(_,index)=>`✓ layout check ${index+1} passed`).join("\n")}\n${complete?"14 pass · 0 fail\n":""}`};
    if(complete) {
      clearInterval(timer);liveTimers.delete(timer);liveResults.delete(result.id);addTool(actor,result);
      emitEvent(actor.summary.id,{type:"tool_finished",message_id:message.id,call_id:result.call_id,name:result.name,result_id:result.id,summary:result.summary,error:null,outcome:result.outcome});
      touchMessage(message,{status:"completed",finished_at_ms:now(),reply:"Output complete."});
      emitEvent(actor.summary.id,{type:"message_updated",message_id:message.id,status:"completed"});setActorStatus(actor,"idle");
    } else emitEvent(actor.summary.id,{type:"tool_updated",message_id:message.id,call_id:result.call_id,result_id:result.id});
  },700);
  liveTimers.add(timer);
}
export async function prepareToolPreviewRead(id: string, activity = false): Promise<boolean> {
  const example = toolExamples.find(item=>`${activity ? "showcase-message" : "showcase-result"}-${item.id}`===id);
  if (example?.network === "loading" && !activity) await new Promise(resolve=>setTimeout(resolve,1200));
  if (example?.network === (activity ? "activity-error" : "detail-error") && !failures.has(id)) { failures.add(id); return false; }
  return true;
}
function seedResources() {
  const sources = [
    {id:"screenshot",type:"image/svg+xml",content:'<svg xmlns="http://www.w3.org/2000/svg" width="640" height="360"><rect width="640" height="360" rx="20" fill="#edf4e9"/><rect x="24" y="24" width="592" height="46" rx="12" fill="#d4e6ca"/><text x="44" y="54" font-family="sans-serif" font-size="20" fill="#244025">Daat Locus · Preview</text><rect x="44" y="110" width="360" height="66" rx="16" fill="white"/><rect x="230" y="210" width="366" height="84" rx="16" fill="#95ec69"/></svg>'},
    {id:"html",type:"text/html",content:"<!doctype html><html lang=\"en\"><meta charset=\"utf-8\"><style>body{font:16px sans-serif;padding:24px;background:#f1f6ed;color:#24351e}table{border-collapse:collapse}td{padding:12px;border:1px solid #b9cbb0}</style><h2>Layout review report</h2><table><tr><td>Phone</td><td>Passed</td></tr><tr><td>Desktop</td><td>Passed</td></tr></table></html>"},
    {id:"report",type:"text/markdown",content:"# Layout review\n\nDesktop and mobile layout checks are complete.\n"},
  ];
  for (const source of sources) {
    const bytes = new TextEncoder().encode(source.content), id=`showcase-${source.id}`;
    store.attachments.set(id,{attachment:{id,name:source.id,media_type:source.type,size_bytes:bytes.length,uri:resource(source.id)},bytes,media_type:source.type});
  }
}
export function seedToolExamples(actor: ActorStore, examples: ToolExample[]) {
  seedResources();
  if (examples.length > 1) { seedCombinedWork(actor,examples); return; }
  const clock = now();
  examples.forEach((example,index)=>{
    const start=clock-(examples.length-index)*10000;
    const status=example.status ?? "completed";
    const message=addMessage(actor,{kind:"request",source:{kind:"actor",actor_id:ROOT_ACTOR_ID},reply_to:{kind:"actor",actor_id:ROOT_ACTOR_ID,request_id:`showcase-${example.id}`},content:example.label,status});
    message.id=`showcase-message-${example.id}`;
    touchMessage(message,{created_at_ms:start,started_at_ms:status==="queued"?null:start+100,finished_at_ms:["running","queued"].includes(status)?null:start+5000,reply:status==="completed"?"The output is retained in the activity record above.":null,error:status==="failed"?example.error:null,final_generation:2});
    const fact=(at:number,event:ActorEvent)=>{const record=emitEvent(actor.summary.id,event);if(record)record.created_at_ms=at;};
    fact(start,{type:"message_added",message_id:message.id});
    if (status==="queued") return;
    fact(start+200,{type:"generation_started",message_id:message.id,generation:1});
    fact(start+300,{type:"generation_delta",message_id:message.id,generation:1,channel:"assistant",text:`Reviewing ${example.label}.`});
    const result=toolExampleResult(example,actor.summary.id);
    fact(start+1000,{type:"tool_started",message_id:message.id,call_id:result.call_id,name:result.name,outcome:result.outcome});
    if (!example.running) {
      addTool(actor,result);
      fact(start+2000,{type:"tool_finished",message_id:message.id,call_id:result.call_id,name:result.name,result_id:result.id,summary:result.summary,error:result.error,outcome:result.outcome});
    } else if(example.name.startsWith("terminal__")||example.outcome&&typeof example.outcome==="object"&&!Array.isArray(example.outcome)&&String(example.outcome.operation).startsWith("terminal__"))seedLiveTerminal(actor,message,result);
    fact(start+5000,{type:"message_updated",message_id:message.id,status});
  });
  setActorStatus(actor,[...examples].some(example=>example.status==="running")?"running":"idle");
}

function seedCombinedWork(actor: ActorStore, examples: ToolExample[]) {
  const settled=examples.filter(example=>!example.running&&example.status!=="queued");
  const start=now()-(settled.length+2)*3000;
  const message=addMessage(actor,{kind:"request",source:{kind:"actor",actor_id:ROOT_ACTOR_ID},reply_to:{kind:"actor",actor_id:ROOT_ACTOR_ID,request_id:"showcase-combined"},content:"Review all tool renderers, including successful, failed, cancelled and malformed results.",status:"completed"});
  message.id="showcase-message-combined";
  const fact=(at:number,event:ActorEvent)=>{const record=emitEvent(actor.summary.id,event);if(record)record.created_at_ms=at;};
  touchMessage(message,{created_at_ms:start,started_at_ms:start,finished_at_ms:start+(settled.length+1)*3000,reply:"All tool examples are collected in this work record.",final_generation:2});
  fact(start,{type:"message_added",message_id:message.id});
  fact(start+100,{type:"generation_started",message_id:message.id,generation:1});
  fact(start+200,{type:"generation_delta",message_id:message.id,generation:1,channel:"reasoning",text:"Check each tool's output and error states."});
  settled.forEach((example,index)=>{
    const at=start+index*3000;
    const result={...toolExampleResult(example,actor.summary.id),message_id:message.id};
    if(example.status==="cancelled")result.error="Call cancelled. Completed operations are retained.";
    if(example.status==="interrupted")result.error="Call interrupted. Completed operations are retained.";
    addTool(actor,result);
    fact(at+1000,{type:"tool_started",message_id:message.id,call_id:result.call_id,name:result.name,outcome:result.outcome});
    fact(at+2000,{type:"tool_finished",message_id:message.id,call_id:result.call_id,name:result.name,result_id:result.id,summary:result.summary,error:result.error,outcome:result.outcome});
  });
  fact(message.finished_at_ms!,{type:"message_updated",message_id:message.id,status:"completed"});
  setActorStatus(actor,"idle");
}
