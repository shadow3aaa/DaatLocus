import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { WorkTranscript } from "../src/components/conversation-turns";
import { workTranscriptParts } from "../src/lib/work-timeline";
import { t } from "../src/lib/i18n";
import type { EventRecord, MessageRecord } from "../src/lib/actor-api";

const input = (id: string, seq: number, started_at_ms: number): MessageRecord => ({id,seq,actor_id:"worker",kind:"request",source:{kind:"user"},reply_to:{kind:"user"},in_reply_to:null,content:id,attachments:[],status:"running",cancel_requested:false,reply:null,error:null,started_at_ms,created_at_ms:started_at_ms,updated_at_ms:started_at_ms});
const first=input("first",1,10_000), second=input("second",2,30_000), latest=input("latest",3,45_000);
const queued: MessageRecord={...input("queued",4,50_000),status:"queued",started_at_ms:null};
const activity = (messages: MessageRecord[], events: EventRecord[] = []) => workTranscriptParts(messages.filter(message=>message.actor_id==="worker"),messages,events).filter(part=>part.kind==="activity");

test("only the serial execution owner ticks while unfinished older inputs freeze at their handoff", () => {
  const messages=[first,second,latest,queued,{...input("foreign",100,80_000),actor_id:"another-actor"}];
  expect(activity(messages).map(part=>[part.message.id,part.span])).toEqual([
    ["first",{start:10_000,end:30_000,running:false}],
    ["second",{start:30_000,end:45_000,running:false}],
    ["latest",{start:45_000,end:null,running:true}],
  ]);
  expect(first.status).toBe("running"); // Suspension must not masquerade as request completion.
});

test("a suspended original becomes active again after nested work completes, despite a later queued input", () => {
  const messages=[first,{...second,status:"completed" as const,finished_at_ms:70_000},{...latest,status:"completed" as const,finished_at_ms:60_000},queued];
  expect(activity(messages).at(-1)).toMatchObject({message:{id:"first"},span:{start:70_000,end:null,running:true}});
  expect(activity(messages).filter(part=>part.span.running)).toHaveLength(1);
});

test("terminal event time freezes a completed counter before the detailed message fetch arrives", () => {
  const done={...first,status:"completed" as const};
  const event: EventRecord={seq:10,actor_id:"worker",created_at_ms:72_000,event:{type:"message_updated",message_id:first.id,status:"completed"}};
  expect(activity([done],[event,{...event,actor_id:"another-actor",created_at_ms:999_000}])[0].span).toEqual({start:10_000,end:72_000,running:false});
  expect(activity([{...done,updated_at_ms:71_000}])[0].span).toEqual({start:10_000,end:71_000,running:false});
});

test("a conversation containing three unfinished inputs renders one working heading and two historical headings", () => {
  const messages=[first,second,latest,queued];
  const chat={messages,events:[],tools:[]} as unknown as Parameters<typeof WorkTranscript>[0]["chat"];
  const html=renderToStaticMarkup(<WorkTranscript messages={messages} chat={chat} access={{kind:"owner",manage:true}} onActor={()=>{}}/>);
  const working=t("conversation_turns.working");
  expect(html.split(working).length-1).toBe(1);
  expect(html.match(/class="dl-round-heading"/g)).toHaveLength(3);
  expect(html.match(/aria-expanded="false"/g)).toHaveLength(2);
});

test("resumed work follows completed nested outcomes without moving or duplicating its input", () => {
  const messages=[first,{...second,status:"completed" as const,finished_at_ms:70_000,reply:"SECOND_RESULT"},{...latest,status:"completed" as const,finished_at_ms:60_000,reply:"LATEST_RESULT"}];
  const parts=workTranscriptParts(messages,messages);
  expect(parts.filter(part=>part.kind==="activity").map(part=>[part.message.id,part.kind==="activity"&&part.span])).toEqual([
    ["first",{start:10_000,end:30_000,running:false}],
    ["second",{start:30_000,end:45_000,running:false}],
    ["latest",{start:45_000,end:60_000,running:false}],
    ["second",{start:60_000,end:70_000,running:false}],
    ["first",{start:70_000,end:null,running:true}],
  ]);
  const chat={messages,events:[],tools:[]} as unknown as Parameters<typeof WorkTranscript>[0]["chat"];
  const html=renderToStaticMarkup(<WorkTranscript messages={messages} chat={chat} access={{kind:"owner",manage:true}} onActor={()=>{}}/>);
  const working=html.indexOf(t("conversation_turns.working"));
  expect(html.indexOf("SECOND_RESULT")).toBeLessThan(working);
  expect(html.indexOf("LATEST_RESULT")).toBeLessThan(working);
  expect(html.match(/id="message-worker-first"/g)).toHaveLength(1);
  expect(html.match(/class="dl-user-text">first</g)).toHaveLength(1);
  expect(html.slice(working)).not.toContain(t("work_round.elapsed_value0",{value0:""}).trim());
});

test("finishing the original input preserves earlier spans and places its final result after nested work", () => {
  const messages=[{...first,status:"completed" as const,finished_at_ms:90_000,reply:"ORIGINAL_RESULT"},{...second,status:"completed" as const,finished_at_ms:70_000,reply:"NESTED_RESULT"}];
  const parts=workTranscriptParts(messages,messages);
  expect(parts.filter(part=>part.kind==="activity").map(part=>part.kind==="activity"&&part.span)).toEqual([
    {start:10_000,end:30_000,running:false},{start:30_000,end:70_000,running:false},{start:70_000,end:90_000,running:false},
  ]);
  expect(parts.filter(part=>part.kind==="outcome").map(part=>part.message.reply)).toEqual(["NESTED_RESULT","ORIGINAL_RESULT"]);
});

test("withdrawn queued inputs and delivery records never split execution", () => {
  const withdrawn={...queued,status:"cancelled" as const,updated_at_ms:60_000};
  const delivery={...input("delivery",5,65_000),kind:"delivery" as const,status:"completed" as const,finished_at_ms:66_000};
  expect(activity([first,withdrawn,delivery]).map(part=>[part.message.id,part.span])).toEqual([["first",{start:10_000,end:null,running:true}]]);
});
