import i18n from "../src/lib/i18n";
import { describe, expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { PrimaryTranscript, PrimaryTurn, WorkTranscript } from "../src/components/conversation-turns";
import { Activities } from "../src/components/conversation-content";
import { Composer } from "../src/components/composer";
import { TooltipProvider } from "../src/components/ui/tooltip";
import { activityTools, isLiveReasoning, mergeActivity, workActivityTimeline, workTimeline } from "../src/lib/work-timeline";
import { clearStoredDrafts, loadDraft, saveDraft } from "../src/lib/conversation-drafts";
import type { EventRecord, MessageRecord } from "../src/lib/actor-api";
const message: MessageRecord = { seq: 1,id:"m",actor_id:"root",source:{kind:"user",},reply_to:{kind:"user",},kind:"request",in_reply_to:null,content:"整理报告",attachments:[],status:"running",cancel_requested:false,reply:null,error:null,created_at_ms:1,updated_at_ms:1 };
const event = (seq:number,event:EventRecord["event"]): EventRecord => ({seq,actor_id:"root",created_at_ms:seq,event});
const memory = (): Storage => { const data = new Map<string,string>(); return { get length(){return data.size;},clear(){data.clear();},getItem(key){return data.get(key)??null;},key(index){return [...data.keys()][index]??null;},removeItem(key){data.delete(key);},setItem(key,value){data.set(key,value);} }; };
describe("fixed presentation and chronological source", () => {
  test("message bubbles distinguish user, user-facing reply and local or remote collaboration", () => {
    const render = (record: MessageRecord) => {
      const chat = {messages:[record],tools:[],approvals:[],events:[]} as unknown as Parameters<typeof WorkTranscript>[0]["chat"];
      return renderToStaticMarkup(<WorkTranscript messages={[record]} chat={chat} access={{kind:"owner",manage:true}} onActor={()=>{}} />);
    };
    const completed = {...message,status:"completed" as const,reply:"RESULT"};
    const user = render(completed);
    expect(user).toContain('dl-bubble-user');
    expect(user).toContain('dl-bubble-assistant');
    expect(user).not.toContain('dl-bubble-actor');
    for (const record of [
      {...completed,source:{kind:"actor" as const,actor_id:"core"},reply_to:{kind:"actor" as const,actor_id:"core",request_id:"request"}},
      {...completed,source:{kind:"remote_actor" as const,stage_id:"peer",actor_id:"core",invocation_id:"call"},reply_to:{kind:"remote" as const,stage_id:"peer",invocation_id:"call",user:false}},
    ]) {
      const html = render(record);
      expect(html.match(/dl-bubble-actor/g)).toHaveLength(1);
      expect(html.match(/dl-bubble-peer/g)).toHaveLength(1);
      expect(html).toContain('dl-work-actor-reply');
      expect(html).not.toContain('dl-bubble-user');
      expect(html).not.toContain('dl-bubble-assistant');
      expect(html).toContain("RESULT");
    }
  });
  test("Core hides Actor communications and tool cards while retaining user deliveries", () => {
    const initial = {...message,actor_id:"core",id:"initial",content:"Delegate work",status:"completed" as const};
    const internal = {...initial,id:"followup",source:{kind:"actor" as const,actor_id:"research"},reply_to:{kind:"user" as const},content:"Internal evidence"};
    const tool = (messageId: string) => ({id:`tool-${messageId}`,message_id:messageId,call_id:messageId,name:"actors__message",summary:"",error:null,status:"completed" as const,outcome:{actor_id:"research",name:"TASK_REFERENCE",message_id:messageId}});
    const chat = {messages:[initial,internal],tools:[tool(initial.id),tool(internal.id)],approvals:[],events:[]} as unknown as Parameters<typeof PrimaryTranscript>[0]["chat"];
    const html = renderToStaticMarkup(<PrimaryTranscript messages={[initial,internal]} working={false} chat={chat} access={{kind:"owner",manage:true}} onActor={()=>{}} />);
    expect(html).not.toContain("TASK_REFERENCE");
    expect(html).not.toContain("dl-actor-reference");
    expect(html).not.toContain("Internal evidence");
    expect(renderToStaticMarkup(<PrimaryTurn message={internal} chat={chat} access={{kind:"owner",manage:true}} onActor={()=>{}} />)).toBe("");
    const reply = renderToStaticMarkup(<PrimaryTurn message={{...internal,reply:"Meaningful user update"}} chat={chat} access={{kind:"owner",manage:true}} onActor={()=>{}} />);
    expect(reply).toContain("Meaningful user update");
    expect(reply).not.toContain("TASK_REFERENCE");
  });
  test("Core has one conversation status after new inputs even while an older input is running", () => {
    const old = {...message,actor_id:"core",content:"OLDER_INPUT"};
    const latest = {...old,id:"latest",status:"queued" as const,content:"LATEST_INPUT"};
    const chat = {messages:[old,latest],tools:[],approvals:[],events:[]} as unknown as Parameters<typeof PrimaryTranscript>[0]["chat"];
    const render = (messages: MessageRecord[], working: boolean, searchQuery?: string) => renderToStaticMarkup(<PrimaryTranscript messages={messages} working={working} chat={chat} access={{kind:"owner",manage:true}} onActor={()=>{}} searchQuery={searchQuery} />);
    const before = render([old], true);
    expect(before.indexOf('role="status"')).toBeGreaterThan(before.indexOf("OLDER_INPUT"));
    const after = render([old,latest], true);
    expect(after.match(/role="status"/g)).toHaveLength(1);
    expect(after.indexOf('role="status"')).toBeGreaterThan(after.indexOf("LATEST_INPUT"));
    expect(render([old,latest], false)).not.toContain('role="status"');
    expect(render([old,latest], true, "")).not.toContain('role="status"');
  });
  test("the shared composer can send a new message while work is running", () => {
    const props: Parameters<typeof Composer>[0] = {value:"New input",onValueChange:()=>{},onSubmit:()=>{},attachments:[],onFiles:()=>{},onRemoveAttachment:()=>{},uploading:false,busy:false,canManage:false,models:[],modelName:"",onModelChange:()=>{}};
    const html = renderToStaticMarkup(<Composer {...props} />);
    expect(html).toContain(`aria-label="${i18n.t("composer.send_message")}"`);
    expect(html).not.toContain(`aria-label="${i18n.t("composer.stop_task")}"`);
    expect(html).not.toContain('disabled=""');
  });
  test("the composer renders exactly one action for its current draft and execution state", () => {
    const props: Parameters<typeof Composer>[0] = {value:"",onValueChange:()=>{},onSubmit:()=>{},attachments:[],onFiles:()=>{},onRemoveAttachment:()=>{},uploading:false,busy:false,canManage:false,models:[],modelName:"",onModelChange:()=>{}};
    const attachment = {id:"file",name:"report.txt",media_type:"text/plain",size_bytes:10,uri:"/v1/actors/root/resources/file"};
    const cases = [
      {patch:{},stop:false,disabled:true},
      {patch:{value:"   "},stop:false,disabled:true},
      {patch:{value:"Continue"},stop:false,disabled:false},
      {patch:{attachments:[attachment]},stop:false,disabled:false},
      {patch:{uploading:true},stop:false,disabled:true},
      {patch:{busy:true},stop:false,disabled:true},
    ];
    for (const {patch,stop,disabled} of cases) {
      const html = renderToStaticMarkup(<Composer {...props} {...patch} />);
      const label = i18n.t(stop ? "composer.stop_task" : "composer.send_message");
      const other = i18n.t(stop ? "composer.send_message" : "composer.stop_task");
      const actions = html.match(new RegExp(`<button[^>]*aria-label="${label}"[^>]*>`, "g"));
      expect(actions).toHaveLength(1);
      expect(actions![0].includes('disabled=""')).toBe(disabled);
      expect(html).not.toContain(`aria-label="${other}"`);
    }
  });
  test("Core shows newly queued user messages before the running input finishes", () => {
    const queued = {...message,id:"new-input",actor_id:"core",status:"queued" as const,content:"NEW_IM_INPUT"};
    const chat = {messages:[message,queued],tools:[],approvals:[],events:[]} as unknown as Parameters<typeof PrimaryTurn>[0]["chat"];
    expect(renderToStaticMarkup(<PrimaryTurn message={queued} chat={chat} access={{kind:"owner",manage:true}} onActor={()=>{}} />)).toContain("NEW_IM_INPUT");
  });


  test("search filters individual conversation sides and work presentation hides intermediate disclosures", () => {
    const record = {...message, content:"UNMATCHED_USER_INPUT", reply:"MATCHED_FINAL_REPLY",status:"completed" as const};
    const chat = {messages:[],tools:[],approvals:[],events:[],toolNext:{}} as unknown as Parameters<typeof PrimaryTurn>[0]["chat"];
    for (const element of [<PrimaryTurn message={record} chat={chat} access={{kind:"owner",manage:true}} onActor={()=>{}} searchQuery="matched_final" />, <WorkTranscript messages={[record]} chat={chat} access={{kind:"owner",manage:true}} onActor={()=>{}} searchQuery="matched_final" />]) {
      const html = renderToStaticMarkup(element);
      expect(html).toContain("MATCHED_FINAL_REPLY");
      expect(html).not.toContain("UNMATCHED_USER_INPUT");
      expect(html).not.toContain(" aria-expanded=");
    }
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
    const work=renderToStaticMarkup(<WorkTranscript messages={[delivery]} chat={chat} access={{kind:"owner",manage:true}} onActor={()=>{}}/>);
    expect(work).toContain(delivery.reply!);
  });
  test("primary omits empty deliveries but retains deliveries containing only a file outcome", () => {
    const delivery: MessageRecord = {...message, id:"file-delivery", source:{kind:"actor",actor_id:"core"}, kind:"delivery", content:"", status:"completed"};
    const chat = {messages:[],tools:[],approvals:[]} as unknown as Parameters<typeof PrimaryTurn>[0]["chat"];
    const render = () => renderToStaticMarkup(<PrimaryTurn message={delivery} chat={chat} access={{kind:"owner",manage:true}} onActor={()=>{}} />);
    expect(render()).toBe("");
    chat.tools = [{id:"file",message_id:delivery.id,call_id:"present",name:"artifacts__present",summary:"",error:null,outcome:{uri:"/v1/actors/root/resources/report",title:"report.txt"}}];
    const html = render();
    expect(html).toContain('href="/v1/actors/root/resources/report"');
    expect(html).toContain("report.txt");
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
  test("reasoning between protocol groups updates one headline while prose separates activity", () => {
    const records = [
      event(1,{type:"generation_delta",message_id:"m",generation:1,channel:"reasoning",text:"HISTORICAL_REASONING"}),
      event(2,{type:"tool_started",message_id:"m",call_id:"read",name:"files__read_file"}),
      event(3,{type:"generation_delta",message_id:"m",generation:2,channel:"reasoning",text:"MORE_HISTORICAL_REASONING"}),
      event(4,{type:"tool_started",message_id:"m",call_id:"edit",name:"files__edit_file"}),
      event(5,{type:"tool_finished",message_id:"m",call_id:"read",name:"files__read_file",result_id:"read-result",summary:"Read",error:null}),
      event(6,{type:"generation_delta",message_id:"m",generation:3,channel:"assistant",text:"PROGRESS_PROSE"}),
      event(7,{type:"tool_started",message_id:"m",call_id:"check",name:"terminal__terminal_exec"}),
      event(8,{type:"generation_delta",message_id:"m",generation:4,channel:"reasoning",text:"Previous paragraph.\n\nChecking the final result."}),
    ];
    expect(workActivityTimeline(records,message)).toEqual([
      {kind:"activity",seq:1,callIds:["read","edit"],thinking:null},
      {kind:"text",seq:6,generation:3,channel:"assistant",content:"PROGRESS_PROSE"},
      {kind:"activity",seq:7,callIds:["check"],thinking:"Checking the final result."},
    ]);
    const toolStarted = event(9,{type:"tool_started",message_id:"m",call_id:"verify",name:"files__read_file"});
    expect(workActivityTimeline([...records,toolStarted],message).at(-1)).toEqual({kind:"activity",seq:7,callIds:["check","verify"],thinking:null});
    expect(workActivityTimeline(records,{...message,status:"completed"}).at(-1)).toEqual({kind:"activity",seq:7,callIds:["check"],thinking:null});
    expect(workActivityTimeline(records,message,false,false).at(-1)).toEqual({kind:"activity",seq:7,callIds:["check"],thinking:null});
  });
  test("thinking without tool calls has no disclosure or historical reasoning body", () => {
    const records = [event(1,{type:"generation_delta",message_id:"m",generation:1,channel:"reasoning",text:"HIDDEN_EARLIER_PARAGRAPH\n\nCurrent thought."})];
    const segments = workActivityTimeline(records,message);
    expect(segments).toEqual([{kind:"activity",seq:1,callIds:[],thinking:"Current thought."}]);
    const html = renderToStaticMarkup(<Activities actorId="root" tools={[]} thinking="Current thought." onActor={()=>{}} />);
    expect(html).toContain('role="status"');
    expect(html).toContain("Current thought.");
    expect(html).not.toContain("<button");
    expect(workActivityTimeline(records,{...message,status:"completed"})).toEqual([]);
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
