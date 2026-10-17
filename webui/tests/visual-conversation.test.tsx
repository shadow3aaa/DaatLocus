import i18n from "../src/lib/i18n";
import { describe, expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { PrimaryTurn, WorkTurn } from "../src/components/conversation-turns";
import { WorkRound } from "../src/components/work-round";
import { MessageQueue } from "../src/components/message-queue";
import { TooltipProvider } from "../src/components/ui/tooltip";
import { activityTools, isLiveReasoning, mergeActivity, workTimeline } from "../src/lib/work-timeline";
import { clearStoredDrafts, loadDraft, saveDraft } from "../src/lib/conversation-drafts";
import type { EventRecord, MessageRecord } from "../src/lib/actor-api";
const message: MessageRecord = { seq: 1,id:"m",actor_id:"root",source:{kind:"user",},reply_to:{kind:"user",},kind:"request",in_reply_to:null,content:"整理报告",attachments:[],status:"running",cancel_requested:false,reply:null,error:null,created_at_ms:1,updated_at_ms:1 };
const event = (seq:number,event:EventRecord["event"]): EventRecord => ({seq,actor_id:"root",created_at_ms:seq,event});
const memory = (): Storage => { const data = new Map<string,string>(); return { get length(){return data.size;},clear(){data.clear();},getItem(key){return data.get(key)??null;},key(index){return [...data.keys()][index]??null;},removeItem(key){data.delete(key);},setItem(key,value){data.set(key,value);} }; };
describe("fixed presentation and chronological source", () => {
  test("queue previews use authorized resource addresses instead of attachment URLs", () => {
    const queued = {...message,status:"queued" as const,attachments:[{id:"image-id",name:"preview.png",media_type:"image/png",size_bytes:42,uri:"https://untrusted.example/private"}]};
    const chat = {queueCursor:null} as unknown as Parameters<typeof MessageQueue>[0]["chat"];
    const html = renderToStaticMarkup(<TooltipProvider><MessageQueue messages={[queued]} active={message} chat={chat} access={{kind:"owner",manage:true}} /></TooltipProvider>);
    expect(html).toContain("/v1/actors/root/resources/image-id");expect(html).not.toContain("untrusted.example");
  });
  test("primary IM excludes live model text, reasoning and tool pipeline", () => {
    const chat = {messages:[],tools:[],approvals:[],generations:{m:{assistant:"INTERNAL_STAGE",reasoning:"INTERNAL_THOUGHT"}},events:[],amend:async()=>{},refresh:async()=>{}} as unknown as Parameters<typeof PrimaryTurn>[0]["chat"];
    const html = renderToStaticMarkup(<PrimaryTurn message={message} chat={chat} access={{kind:"owner",manage:true}} onActor={()=>{}} />);
    expect(html).toContain("整理报告"); expect(html).not.toContain("INTERNAL_STAGE"); expect(html).not.toContain("INTERNAL_THOUGHT");
    const delivered = renderToStaticMarkup(<PrimaryTurn message={{...message,status:"completed",reply:"明确给用户的结果"}} chat={chat} access={{kind:"owner",manage:true}} onActor={()=>{}} />);
    expect(delivered).toContain("明确给用户的结果");
  });
  test("both conversation views render a correlated delivery result", () => {
    const chat = {messages:[],tools:[],approvals:[],events:[],toolNext:{}} as unknown as Parameters<typeof PrimaryTurn>[0]["chat"];
    const delivery:MessageRecord = {...message,id:"delivery",source:{kind:"actor",actor_id:"core"},kind:"delivery",in_reply_to:"m",content:"",status:"completed",reply:"任务已交给项目 Actor"};
    const primary=renderToStaticMarkup(<PrimaryTurn message={delivery} chat={chat} access={{kind:"owner",manage:true}} onActor={()=>{}}/>);
    expect(primary).toContain(delivery.reply!);
    const work=renderToStaticMarkup(<WorkTurn message={delivery} chat={chat} access={{kind:"owner",manage:true}} onActor={()=>{}}/>);
    expect(work).toContain(delivery.reply!);
  });
  test("prose interleaves with actual tool groups; final generation is not duplicated", () => {
    const records = [event(1,{type:"generation_started",message_id:"m",generation:1}),event(2,{type:"generation_delta",message_id:"m",generation:1,channel:"assistant",text:"先检查"}),event(3,{type:"tool_started",message_id:"m",call_id:"c",name:"files__read_file"}),event(4,{type:"generation_started",message_id:"m",generation:2}),event(5,{type:"generation_delta",message_id:"m",generation:2,channel:"assistant",text:"结果"})];
    expect(workTimeline(records,{...message,status:"completed",reply:"结果",final_generation:2}).map(item=>item.kind)).toEqual(["text","tools"]);
    expect(mergeActivity(records,[records[2]],"root","m")).toHaveLength(5);
    expect(workTimeline(records.slice(0,3),{...message,status:"completed",reply:"结果",final_generation:2},false)[0]).toMatchObject({content:"先检查"});
  });
  test("reopening an active tool restores its row and ends the preceding thinking state", () => {
    const records = [event(1,{type:"generation_delta",message_id:"m",generation:1,channel:"reasoning",text:"THOUGHT"}),event(2,{type:"tool_started",message_id:"m",call_id:"terminal",name:"terminal__terminal_exec"})];
    const segments=workTimeline(records,message);
    expect(activityTools(records,message,[])).toMatchObject([{call_id:"terminal",status:"running"}]);
    expect(activityTools(records,message,[],false)).toMatchObject([{call_id:"terminal",status:"unknown"}]);
    expect(isLiveReasoning(segments[0],0,segments,true)).toBe(false);
    expect(isLiveReasoning(segments[0],0,segments.slice(0,1),true)).toBe(true);
    const finished=event(3,{type:"tool_finished",message_id:"m",call_id:"terminal",name:"terminal__terminal_exec",result_id:"result",summary:"Done",error:null});
    expect(activityTools([...records,finished],message,[])).toMatchObject([{id:"result",status:"completed"}]);
    expect(activityTools(records,{...message,status:"interrupted"},[])).toMatchObject([{status:"completed",error:i18n.t("conversation_content.interrupted")}]);
  });
  test("completed activity renders collapsed with intermediate prose hidden", () => {
    const completed = {...message,status:"completed" as const,started_at_ms:10_000,finished_at_ms:72_000,reply:"结果",final_generation:2};
    const html = renderToStaticMarkup(<WorkRound message={completed} events={[event(1,{type:"generation_delta",message_id:"m",generation:1,channel:"assistant",text:"INTERMEDIATE_TEXT"})]} tools={[]} onMoreTools={async()=>{}} onActor={()=>{}}/>);
    expect(html).toContain('aria-expanded="false"');
    expect(html).not.toContain("INTERMEDIATE_TEXT");
  });
});
describe("durable drafts are identity/Actor partitioned data, never credentials", () => {
  test("drafts and pending sends retain more than four attachment references", () => {
    const store = memory();
    const attachments = Array.from({length: 6}, (_, index) => ({id: `attachment-${index}`, name: `File ${index}`, media_type: "text/plain", size_bytes: index, uri: "https://unsafe.example/file"}));
    const ids = attachments.map(item => item.id);
    saveDraft("owner:id:root", "root", {text: "draft", attachments, pending: {content: "draft", attachments: ids, key: "operation-key"}}, store, 100);
    const recovered = loadDraft("owner:id:root", "root", store, 101)!;
    expect(recovered.attachments.map(item => item.id)).toEqual(ids);
    expect(recovered.pending?.attachments).toEqual(ids);
  });
  test("refresh restores text, safe references and the pending operation key", () => {
    const store=memory(); saveDraft("owner:public-id:root","root",{text:"草稿",attachments:[{id:"opaque-id",name:"报告.txt",media_type:"text/plain",size_bytes:12,uri:"https://unsafe.example/file"}],pending:{content:"草稿",attachments:["opaque-id"],key:"operation-key"}},store,100);
    const recovered=loadDraft("owner:public-id:root","root",store,101)!;
    expect(recovered.text).toBe("草稿"); expect(recovered.attachments[0].uri).toBe("/v1/actors/root/resources/opaque-id"); expect(recovered.pending?.key).toBe("operation-key");
    expect(loadDraft("owner:another-id:root","root",store,101)).toBeUndefined(); expect(loadDraft("owner:public-id:root","different",store,101)).toBeUndefined();
  });
  test("expiry and logout remove private draft entries without touching preferences", () => { const store=memory();store.setItem("preferences","safe");saveDraft("owner:id:root","root",{text:"private",attachments:[]},store,100);expect(loadDraft("owner:id:root","root",store,100+13*3600_000)).toBeUndefined();saveDraft("owner:another:root","root",{text:"private",attachments:[]},store,100);clearStoredDrafts(store);expect(store.length).toBe(1);expect(store.getItem("preferences")).toBe("safe"); });
  test("malformed expiration cannot keep a private draft forever", () => { const store=memory();saveDraft("owner:id:root","root",{text:"private",attachments:[]},store,100);const key=store.key(0)!;const data=JSON.parse(store.getItem(key)!);data.expires_at="not a timestamp";store.setItem(key,JSON.stringify(data));expect(loadDraft("owner:id:root","root",store,101)).toBeUndefined();expect(store.length).toBe(0); });
});
