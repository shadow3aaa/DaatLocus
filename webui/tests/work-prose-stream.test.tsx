import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { Markdown } from "../src/components/markdown";
import { workActivityTimeline } from "../src/lib/work-timeline";
import type { EventRecord, MessageRecord } from "../src/lib/actor-api";

const message: MessageRecord = {seq:1,id:"task",actor_id:"worker",kind:"request",source:{kind:"user"},reply_to:{kind:"user"},in_reply_to:null,content:"Check the build",attachments:[],status:"running",cancel_requested:false,reply:null,error:null,created_at_ms:0,updated_at_ms:0};
const delta = (seq:number, channel:"assistant"|"reasoning", text:string, generation=1):EventRecord => ({seq,actor_id:"worker",created_at_ms:seq,event:{type:"generation_delta",message_id:"task",generation,channel,text}});

test("interleaved reasoning flushes preserve a continuous assistant paragraph and Markdown", () => {
  const records = [
    delta(1,"assistant","QQ"),
    delta(2,"reasoning","Checking the device conversation."),
    delta(3,"assistant",' has a "我的手机" device-transfer conversation. **Now'),
    delta(4,"reasoning","Waiting for the build."),
    delta(5,"assistant"," let me wait** on the build.\n\n下一段保持原有换行。"),
    delta(6,"reasoning","Check the final result."),
  ];
  for (const status of ["running","interrupted"] as const) {
    const segments = workActivityTimeline(records,{...message,status});
    const prose = segments.filter(segment => segment.kind === "text");
    expect(prose).toHaveLength(1);
    expect(prose[0].content).toBe('QQ has a "我的手机" device-transfer conversation. **Now let me wait** on the build.\n\n下一段保持原有换行。');
    const html = renderToStaticMarkup(<Markdown content={prose[0].content} onActor={()=>{}} />);
    expect(html.match(/<p>/g)).toHaveLength(2);
    expect(html).toContain("<strong>Now let me wait</strong>");
    expect(segments.at(-1)?.kind).toBe(status === "running" ? "activity" : "text");
  }
});

test("real tool and generation boundaries still separate assistant prose", () => {
  const tool:EventRecord = {seq:4,actor_id:"worker",created_at_ms:4,event:{type:"tool_started",message_id:"task",call_id:"read",name:"files__read_file"}};
  const segments = workActivityTimeline([
    delta(1,"assistant","Before"),delta(2,"reasoning","Thought"),delta(3,"assistant"," tool."),
    tool,delta(5,"assistant","After tool."),delta(6,"reasoning","Thought"),delta(7,"assistant","Next generation.",2),
  ],message);
  expect(segments.map(segment => segment.kind === "text" ? segment.content : segment.kind)).toEqual(["Before tool.","activity","After tool.","Next generation."]);
});

test("a live thinking headline uses its channel's complete text across interleaved flushes", () => {
  const records = [
    delta(1,"reasoning","Earlier paragraph.\n\nChecking"),
    delta(2,"assistant","QQ"),
    delta(3,"reasoning"," the build"),
    delta(4,"assistant"," is available."),
    delta(5,"reasoning","."),
  ];
  expect(workActivityTimeline(records,message).at(-1)).toMatchObject({kind:"activity",thinking:"Checking the build."});
  const tool:EventRecord = {seq:6,actor_id:"worker",created_at_ms:6,event:{type:"tool_started",message_id:"task",call_id:"build",name:"terminal__terminal_exec"}};
  expect(workActivityTimeline([...records,tool,delta(7,"reasoning","New thought.")],message).at(-1)).toMatchObject({kind:"activity",thinking:"New thought."});
  expect(workActivityTimeline([...records,delta(6,"reasoning","Next generation.",2)],message).at(-1)).toMatchObject({kind:"activity",thinking:"Next generation."});
});
