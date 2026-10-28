import "../src/lib/i18n";
import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { CommunicationBubble, communicationActor } from "../src/components/actor-message-bubble";
import { workActivityTimeline, workTranscriptParts } from "../src/lib/work-timeline";
import type { EventRecord, MessageRecord } from "../src/lib/actor-api";

const message: MessageRecord = {seq:1,id:"request",actor_id:"iris",source:{kind:"actor",actor_id:"core"},reply_to:{kind:"actor",actor_id:"core",request_id:"original"},kind:"request",in_reply_to:null,content:"Check the layout",attachments:[],status:"running",cancel_requested:false,reply:null,error:null,created_at_ms:100,updated_at_ms:100,started_at_ms:100};
const event = (seq: number, event: EventRecord["event"]): EventRecord => ({seq,actor_id:"iris",created_at_ms:100+seq,event});

test("communication captions distinguish direction and leave ordinary assistant bubbles unmarked", () => {
  for (const direction of ["incoming","outgoing"] as const) {
    const html=renderToStaticMarkup(<CommunicationBubble actorId="Atlas" direction={direction} onActor={()=>{}}><p>Original text</p></CommunicationBubble>);
    expect(html).toContain('role="link"');
    expect(html).toContain('tabindex="0"');
    expect(html).toContain(direction === "incoming" ? "Sent by Atlas" : "Sent to Atlas");
    expect(html).not.toContain("<button");
  }
  expect(renderToStaticMarkup(<CommunicationBubble actorId={null} direction="incoming" onActor={()=>{}}>Assistant prose</CommunicationBubble>)).not.toContain("dl-actor-message");
});

test("remote replies resolve the exact invocation through correlation rather than guessing by device", () => {
  const source: MessageRecord={...message,source:{kind:"remote_actor",actor_id:"Atlas",stage_id:"device",invocation_id:"first"},reply_to:{kind:"remote",stage_id:"device",invocation_id:"first",user:false}};
  expect(communicationActor(source,"incoming")).toBe("device~Atlas");
  expect(communicationActor(source,"outgoing")).toBe("device~Atlas");
  const unrelated={...source,id:"unrelated",source:{...source.source,invocation_id:"other"}} as MessageRecord;
  expect(communicationActor(unrelated,"outgoing")).toBeNull();
  expect(communicationActor({...unrelated,in_reply_to:source.id},"outgoing",[source])).toBe("device~Atlas");
  expect(communicationActor({...unrelated,in_reply_to:unrelated.id},"outgoing",[unrelated])).toBeNull();
});

test("successful sends split chronological tool groups once, while failed sends remain inspectable tools", () => {
  const records=[
    event(1,{type:"tool_started",message_id:"request",call_id:"create",name:"actors__create"}),
    event(2,{type:"tool_started",message_id:"request",call_id:"send",name:"actors__message"}),
    event(3,{type:"tool_finished",message_id:"request",call_id:"send",name:"actors__message",result_id:"sent",summary:"Delivered",error:null,outcome:{sent_message:true}}),
    event(4,{type:"tool_started",message_id:"request",call_id:"status",name:"actors__status"}),
    event(5,{type:"tool_finished",message_id:"request",call_id:"failed",name:"actors__message",result_id:"failed",summary:"Failed",error:"Actor archived"}),
  ];
  expect(workActivityTimeline(records,message)).toEqual([
    {kind:"activity",seq:1,callIds:["create"],thinking:null},
    {kind:"message",seq:3,resultId:"sent"},
    {kind:"activity",seq:4,callIds:["status","failed"],thinking:null},
  ]);
  // A resumed input may finish a send after its original range has ended.
  // The old range must not retain an additional sent-message tool row.
  expect(workActivityTimeline(records.slice(0,2),message,true,false,records)).toEqual([
    {kind:"activity",seq:1,callIds:["create"],thinking:null},
  ]);
});

test("an outward delivery sits between the surrounding active execution ranges", () => {
  const delivery: MessageRecord={...message,seq:2,id:"delivery",kind:"delivery",content:"",reply:"Progress",status:"completed",created_at_ms:200,finished_at_ms:200};
  const parts=workTranscriptParts([message,delivery],[message,delivery]);
  expect(parts.filter(part=>part.kind==="activity").map(part=>part.kind==="activity"&&part.range)).toEqual([{start:100,end:200,live:false},{start:200,end:null,live:true}]);
  const outcome=parts.findIndex(part=>part.kind==="outcome"&&part.message.id==="delivery");
  expect(outcome).toBeGreaterThan(parts.findIndex(part=>part.kind==="activity"));
  expect(outcome).toBeLessThan(parts.findLastIndex(part=>part.kind==="activity"));
});
