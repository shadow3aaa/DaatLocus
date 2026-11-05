import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { WorkTranscript } from "../src/components/conversation-turns";
import { WorkActivity } from "../src/components/work-activity";
import { activityRecords, workActivityTimeline, workTranscriptParts } from "../src/lib/work-timeline";
import type { EventRecord, MessageRecord } from "../src/lib/actor-api";

const input = (id: string, seq: number, started_at_ms: number): MessageRecord => ({id,seq,actor_id:"worker",kind:"request",source:{kind:"user"},reply_to:{kind:"user"},in_reply_to:null,content:id,attachments:[],status:"running",cancel_requested:false,reply:null,error:null,started_at_ms,created_at_ms:started_at_ms,updated_at_ms:started_at_ms});
const first=input("first",1,10_000), second=input("second",2,30_000), latest=input("latest",3,45_000);
const queued: MessageRecord={...input("queued",4,50_000),status:"queued",started_at_ms:null};
const activity = (messages: MessageRecord[], events: EventRecord[] = []) => workTranscriptParts(messages.filter(message=>message.actor_id==="worker"),messages,events).filter(part=>part.kind==="activity");

test("work messages render bubbles without an outer activity disclosure or clock", () => {
  const messages=[first,second,latest,queued];
  const chat={messages,events:[],tools:[]} as unknown as Parameters<typeof WorkTranscript>[0]["chat"];
  const html=renderToStaticMarkup(<WorkTranscript messages={messages} chat={chat} access={{kind:"owner",manage:true}} onActor={()=>{}}/>);
  expect(html.match(/dl-bubble dl-work-prompt/g)).toHaveLength(4);
  expect(html).not.toContain("dl-round");
  expect(html).not.toContain("dl-work-round");
  expect(html).not.toContain("aria-expanded=");
});

test("buffered inputs never hide or duplicate another input's tools, before or after completion", () => {
  for (const status of ["running","completed"] as const) {
    const messages=[{...first,status,finished_at_ms:status==="completed"?90_000:null},{...second,created_at_ms:25_000,status,finished_at_ms:status==="completed"?90_000:null},{...latest,status,finished_at_ms:status==="completed"?90_000:null}];
    const events: EventRecord[] = [
      {seq:1,actor_id:"worker",created_at_ms:20_000,event:{type:"tool_started",message_id:first.id,call_id:"early",name:"files__read_file"}},
      {seq:2,actor_id:"worker",created_at_ms:27_000,event:{type:"tool_started",message_id:first.id,call_id:"buffered",name:"files__read_file"}},
      {seq:3,actor_id:"worker",created_at_ms:40_000,event:{type:"tool_finished",message_id:first.id,call_id:"early",name:"files__read_file",result_id:"early",summary:"Read",error:null}},
      {seq:4,actor_id:"worker",created_at_ms:50_000,event:{type:"tool_started",message_id:first.id,call_id:"late",name:"files__read_file"}},
      {seq:5,actor_id:"worker",created_at_ms:80_000,event:{type:"tool_started",message_id:second.id,call_id:"other",name:"files__read_file"}},
      {seq:6,actor_id:"worker",created_at_ms:90_000,event:{type:"tool_finished",message_id:first.id,call_id:"late",name:"files__read_file",result_id:"late",summary:"Read",error:null}},
    ];
    const parts=workTranscriptParts(messages,messages,events);
    const spans=parts.filter(part=>part.kind==="activity");
    const shown=spans.flatMap(part=>activityRecords(events,part.message,part.range));
    expect(shown.map(record=>record.seq).sort((a,b)=>a-b)).toEqual(events.map(record=>record.seq));
    const calls=spans.flatMap(part=>workActivityTimeline(activityRecords(events,part.message,part.range),part.message,true,part.range.live,events).flatMap(segment=>segment.kind==="activity"?segment.callIds:[]));
    expect(calls).toEqual(["early","buffered","late","other"]);
    const secondInput=parts.findIndex(part=>part.kind==="input"&&part.message.id===second.id);
    const early=parts.findIndex(part=>part.kind==="activity"&&activityRecords(events,part.message,part.range).some(record=>record.seq===1));
    const late=parts.findIndex(part=>part.kind==="activity"&&activityRecords(events,part.message,part.range).some(record=>record.seq===2));
    expect(early).toBeLessThan(secondInput);
    expect(late).toBeGreaterThan(secondInput);
    const chat={messages,events:[],tools:[]} as unknown as Parameters<typeof WorkTranscript>[0]["chat"];
    const html=renderToStaticMarkup(<WorkTranscript messages={messages} chat={chat} access={{kind:"owner",manage:true}} onActor={()=>{}}/>);
    expect(html.match(/id="message-worker-first"/g)).toHaveLength(1);
    expect(html.match(/class="dl-user-text">first</g)).toHaveLength(1);
  }
});

test("withdrawn queued inputs and delivery records never split execution", () => {
  const withdrawn={...queued,status:"cancelled" as const,updated_at_ms:60_000};
  const delivery={...input("delivery",5,65_000),kind:"delivery" as const,status:"completed" as const,finished_at_ms:66_000};
  expect(activity([first,withdrawn,delivery]).map(part=>[part.message.id,part.range])).toEqual([["first",{start:10_000,end:null,live:true}]]);
});

test("activity keeps paging when an unfinished page reaches the completion timestamp", () => {
  const message={...first,status:"completed" as const,finished_at_ms:30_000};
  const history:EventRecord[]=[{seq:1,actor_id:"worker",created_at_ms:30_000,event:{type:"tool_started",message_id:first.id,call_id:"last",name:"files__read_file"}}];
  const data={history,cursor:1 as number|null,loading:false,error:"",more:async()=>{},reload:()=>{}};
  const render=()=>renderToStaticMarkup(<WorkActivity message={message} range={{start:10_000,end:30_000,live:false}} history={data} tools={[]} onActor={()=>{}}/>);
  expect(render()).toContain("data-lazy-page");
  data.cursor=null;
  expect(render()).not.toContain("data-lazy-page");
});
