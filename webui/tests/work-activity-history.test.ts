import { expect, test } from "bun:test";
import { conversationReducer, initialConversation } from "../src/hooks/use-actor-conversation";
import { activityTools, extendActivityHistory, mergeActivity, workActivityTimeline } from "../src/lib/work-timeline";
import { activityLabel, approvalNotice } from "../src/lib/tool-presentation";
import type { EventRecord, MessageRecord } from "../src/lib/actor-api";

const message: MessageRecord = {seq:1,id:"request",actor_id:"worker",kind:"request",source:{kind:"user"},reply_to:{kind:"user"},in_reply_to:null,content:"Check both areas",attachments:[],status:"running",cancel_requested:false,reply:null,error:null,created_at_ms:1,updated_at_ms:1};
const record = (seq: number, event: EventRecord["event"]): EventRecord => ({seq,actor_id:"worker",created_at_ms:seq,event});
const prose = (seq: number, text: string) => record(seq,{type:"generation_delta",message_id:"request",generation:seq,channel:"assistant",text});
const started = (seq: number, call: string) => record(seq,{type:"tool_started",message_id:"request",call_id:call,name:"files__read_file"});
const finished = (seq: number, call: string) => record(seq,{type:"tool_finished",message_id:"request",call_id:call,name:"files__read_file",result_id:`result-${call}`,summary:"Read",error:null});
const prefix = [prose(1,"FIRST_AREA"),started(2,"first"),finished(3,"first"),prose(4,"SECOND_AREA")];
const later = [
  started(5,"middle"),finished(6,"middle"),prose(7,"THIRD_AREA"),
  ...Array.from({length:400},(_,index)=>record(index+8,{type:"generation_delta",message_id:"request",generation:7,channel:"reasoning",text:"Checking.\n\n"})),
  started(408,"second"),finished(409,"second"),
];

test("a live approval rejection keeps its reason after history replaces the event window", () => {
  const outcome = {approval:{decision:"denied",reason:"The task does not authorize this file",approval_id:"approval",stage_id:"source",mode:"auto"}};
  const event = record(2,{type:"tool_finished",message_id:"request",call_id:"denied",name:"files__read_file",result_id:"result-denied",summary:"App operation was not approved",error:null,outcome});
  const live = activityTools([event],message,[])[0];
  const restored = activityTools([],message,[{seq:2,id:"result-denied",message_id:"request",call_id:"denied",name:"files__read_file",summary:"App operation was not approved",error:null,outcome}])[0];
  expect(live.status).toBe("completed");
  for (const tool of [live,restored]) {
    expect(tool.error).toBeNull();
    expect(activityLabel(tool).detail).toBe("The task does not authorize this file");
  }
});

test("permission references and grants survive live events being replaced by history pages", () => {
  for (const decision of ["application_required", "approved"] as const) {
    const outcome = {approval:{decision,approval_id:"approval",stage_id:"source",mode:"auto"}};
    const summary = {seq:2,id:"permission-result",message_id:"request",call_id:"permission-call",name:"actors__request_approval",summary:"Permission state",error:null,outcome};
    const event = record(2,{type:"tool_finished",message_id:summary.message_id,call_id:summary.call_id,name:summary.name,result_id:summary.id,summary:summary.summary,error:null,outcome});
    let state = conversationReducer(initialConversation("worker"),{type:"event",record:event});
    const live = activityTools(state.events,message,state.tools)[0];
    state = conversationReducer(state,{type:"tools",actorId:"worker",messageId:"request",nextCursor:null,tools:[summary]});
    const restored = activityTools([],message,state.tools)[0];
    for (const tool of [live,restored]) {
      expect(tool.error).toBeNull();
      expect(tool.outcome).toEqual(outcome);
      expect(approvalNotice(tool.outcome)?.kind).toBe(decision);
    }
  }
});

test("expanded activity preserves completed groups when the conversation stream drops its old window", () => {
  let state = initialConversation("worker");
  let history: EventRecord[] = [];
  let through = 0;
  for (const event of [...prefix,...later]) {
    state = conversationReducer(state,{type:"event",record:event});
    // Seed the historical read through the first page, then consume live windows.
    if (event.seq === 4) {history = prefix; through = 4;}
    if (event.seq > 4) {
      const next = extendActivityHistory(history,state.events,"worker","request",through);
      expect(next.gap).toBe(false);
      history = next.records; through = next.through;
    }
  }
  expect(state.events.some(item=>item.seq===2)).toBe(false);
  expect(workActivityTimeline(history,message).map(segment=>segment.kind==="text"?segment.content:segment.callIds)).toEqual(["FIRST_AREA",["first"],"SECOND_AREA",["middle"],"THIRD_AREA",["second"]]);
  expect(activityTools(history,message,[]).map(tool=>[tool.call_id,tool.status,tool.error])).toEqual([["first","completed",null],["middle","completed",null],["second","completed",null]]);
});

test("a disconnected tail waits for incremental backfill instead of appearing before the missing history", () => {
  const tail = later.slice(-20);
  const missing = extendActivityHistory(prefix,tail,"worker","request",4);
  expect(missing.gap).toBe(true);
  expect(missing.through).toBe(4);
  expect(missing.records).toEqual(prefix);
  // The activity endpoint returns the missing prefix after cursor 4. No restart
  // from zero or guessed placement from tool-summary sequence numbers is needed.
  const restored = mergeActivity(missing.records,later,"worker","request");
  const next = extendActivityHistory(restored,[...tail,prose(410,"NEXT_AREA"),started(411,"third")],"worker","request",409);
  expect(next.gap).toBe(false);
  const segments = workActivityTimeline(next.records,message);
  expect(segments.filter(segment=>segment.kind==="activity").map(segment=>segment.callIds)).toEqual([["first"],["middle"],["second"],["third"]]);
  expect(activityTools(next.records,message,[]).map(tool=>[tool.call_id,tool.status])).toEqual([["first","completed"],["middle","completed"],["second","completed"],["third","running"]]);
});

test("replayed live windows do not duplicate tools or advance past unseen activity", () => {
  const next = extendActivityHistory(prefix,prefix,"worker","request",4);
  expect(next.records).toBe(prefix);
  expect(next.through).toBe(4);
  const foreign = {...started(9,"foreign"),actor_id:"other"};
  expect(extendActivityHistory(prefix,[...prefix,foreign],"worker","request",4).records).toBe(prefix);
});
