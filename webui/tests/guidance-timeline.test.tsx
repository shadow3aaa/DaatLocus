import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { workActivityTimeline } from "../src/lib/work-timeline";
import { WorkRound } from "../src/components/work-round";
import { PrimaryTranscript } from "../src/components/conversation-turns";
import type { EventRecord, MessageRecord } from "../src/lib/actor-api";

const message: MessageRecord = {seq:1,id:"task",actor_id:"worker",kind:"request",source:{kind:"user"},reply_to:{kind:"user"},in_reply_to:null,content:"ORIGINAL_INPUT",attachments:[],status:"running",cancel_requested:false,reply:null,error:null,created_at_ms:100,updated_at_ms:100};
const guide: MessageRecord = {...message,id:"guide",seq:2,kind:"steering",in_reply_to:"task",status:"completed",content:"OWNER_GUIDANCE",created_at_ms:250,updated_at_ms:900,finished_at_ms:900};
const event = (seq:number,event:EventRecord["event"]):EventRecord => ({seq,actor_id:"worker",created_at_ms:seq*100,event});
const text = (seq:number,content:string) => event(seq,{type:"generation_delta",message_id:"task",generation:1,channel:"assistant",text:content});
const start = (seq:number,call_id:string) => event(seq,{type:"tool_started",message_id:"task",call_id,name:"files__read_file"});
const finish = (seq:number,call_id:string) => event(seq,{type:"tool_finished",message_id:"task",call_id,name:"files__read_file",result_id:call_id,summary:"Read",error:null});
const summarize = (events:EventRecord[],guidance:MessageRecord[],complete=true) => workActivityTimeline(events,message,complete,true,guidance).map(item => item.kind === "guidance" ? item.message.content : item.kind === "text" ? item.content : item.callIds);

test("guidance uses its send time to split assistant text, rather than its later application time", () => {
  expect(summarize([text(1,"BEFORE"),text(3,"AFTER")],[guide])).toEqual(["BEFORE","OWNER_GUIDANCE","AFTER"]);
});
test("a guide separates tool groups without moving an earlier call when its result arrives later", () => {
  expect(summarize([start(1,"first"),start(3,"second"),finish(4,"first"),finish(5,"second")],[guide])).toEqual([["first"],"OWNER_GUIDANCE",["second"]]);
});
test("partial history waits until the guide's position is loaded and filters unrelated or unapplied guides", () => {
  expect(summarize([text(1,"BEFORE")],[guide],false)).toEqual(["BEFORE"]);
  expect(summarize([text(1,"BEFORE"),text(3,"AFTER")],[guide],false)).toEqual(["BEFORE","OWNER_GUIDANCE","AFTER"]);
  expect(summarize([text(1,"BEFORE"),text(3,"AFTER")],[{...guide,actor_id:"other"},{...guide,in_reply_to:"other-task"},{...guide,status:"queued"}])).toEqual(["BEFOREAFTER"]);
});
test("collapsing a completed round retains the owner's guidance as visible conversation content", () => {
  const html = renderToStaticMarkup(<WorkRound message={{...message,status:"completed"}} events={[]} tools={[]} guidance={[guide]} onActor={()=>{}} />);
  expect(html).toContain("OWNER_GUIDANCE");
  expect(html.indexOf("OWNER_GUIDANCE")).toBeGreaterThan(html.indexOf('aria-expanded="false"'));
});
test("Core renders guidance once at its own chronological message position", () => {
  const original = {...message,actor_id:"core",status:"completed" as const,reply:"EARLIER_REPLY"};
  const latest = {...guide,actor_id:"core"};
  const chat = {messages:[original,latest],tools:[],approvals:[],events:[]} as unknown as Parameters<typeof PrimaryTranscript>[0]["chat"];
  const html = renderToStaticMarkup(<PrimaryTranscript messages={[original,latest]} working={false} chat={chat} access={{kind:"owner",manage:true}} onActor={()=>{}} />);
  expect(html.match(/OWNER_GUIDANCE/g)).toHaveLength(1);
  expect(html.indexOf("OWNER_GUIDANCE")).toBeGreaterThan(html.indexOf("EARLIER_REPLY"));
});
