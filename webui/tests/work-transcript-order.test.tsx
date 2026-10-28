import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { WorkTranscript } from "../src/components/conversation-turns";
import { workTranscriptParts } from "../src/lib/work-timeline";
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

test("resumed work follows completed nested outcomes without moving or duplicating its input", () => {
  const messages=[first,{...second,status:"completed" as const,finished_at_ms:70_000,reply:"SECOND_RESULT"},{...latest,status:"completed" as const,finished_at_ms:60_000,reply:"LATEST_RESULT"}];
  const parts=workTranscriptParts(messages,messages);
  expect(parts.filter(part=>part.kind==="activity").map(part=>[part.message.id,part.kind==="activity"&&part.range])).toEqual([
    ["first",{start:10_000,end:30_000,live:false}],
    ["second",{start:30_000,end:45_000,live:false}],
    ["latest",{start:45_000,end:60_000,live:false}],
    ["second",{start:60_000,end:70_000,live:false}],
    ["first",{start:70_000,end:null,live:true}],
  ]);
  const chat={messages,events:[],tools:[]} as unknown as Parameters<typeof WorkTranscript>[0]["chat"];
  const html=renderToStaticMarkup(<WorkTranscript messages={messages} chat={chat} access={{kind:"owner",manage:true}} onActor={()=>{}}/>);
  expect(html).not.toContain("dl-round-heading");
  expect(html).not.toContain("dl-work-round");
  expect(html.match(/id="message-worker-first"/g)).toHaveLength(1);
  expect(html.match(/class="dl-user-text">first</g)).toHaveLength(1);
});

test("finishing the original input preserves earlier spans and places its final result after nested work", () => {
  const messages=[{...first,status:"completed" as const,finished_at_ms:90_000,reply:"ORIGINAL_RESULT"},{...second,status:"completed" as const,finished_at_ms:70_000,reply:"NESTED_RESULT"}];
  const parts=workTranscriptParts(messages,messages);
  expect(parts.filter(part=>part.kind==="activity").map(part=>part.kind==="activity"&&part.range)).toEqual([
    {start:10_000,end:30_000,live:false},{start:30_000,end:70_000,live:false},{start:70_000,end:90_000,live:false},
  ]);
  expect(parts.filter(part=>part.kind==="outcome").map(part=>part.message.reply)).toEqual(["NESTED_RESULT","ORIGINAL_RESULT"]);
});

test("withdrawn queued inputs and delivery records never split execution", () => {
  const withdrawn={...queued,status:"cancelled" as const,updated_at_ms:60_000};
  const delivery={...input("delivery",5,65_000),kind:"delivery" as const,status:"completed" as const,finished_at_ms:66_000};
  expect(activity([first,withdrawn,delivery]).map(part=>[part.message.id,part.range])).toEqual([["first",{start:10_000,end:null,live:true}]]);
});
