import { coreConnectionMessage, type CoreStatus } from "../src/lib/stage-api";
import i18n from "../src/lib/i18n";
import { beforeEach, describe, expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { PrimaryTurn, WorkTurn } from "../src/components/conversation-turns";
import { WorkRound } from "../src/components/work-round";
import { MessageQueue } from "../src/components/message-queue";
import { Composer } from "../src/components/composer";
import { TooltipProvider } from "../src/components/ui/tooltip";
import { FileChangeView } from "../src/components/file-change";
import { ProvidersSettings } from "../src/components/settings/providers-settings";
import { elapsedLabel, isPrimaryActor, mergeActivity, workTimeline } from "../src/lib/work-timeline";
import { clearStoredDrafts, loadDraft, saveDraft } from "../src/lib/conversation-drafts";
import type { EventRecord, MessageRecord } from "../src/lib/actor-api";
const message: MessageRecord = { seq: 1,id:"m",actor_id:"root",source:{kind:"user",},reply_to:{kind:"user",},kind:"request",in_reply_to:null,content:"整理报告",attachments:[],status:"running",cancel_requested:false,reply:null,error:null,created_at_ms:1,updated_at_ms:1 };
const event = (seq:number,event:EventRecord["event"]): EventRecord => ({seq,actor_id:"root",created_at_ms:seq,event});
const memory = (): Storage => { const data = new Map<string,string>(); return { get length(){return data.size;},clear(){data.clear();},getItem(key){return data.get(key)??null;},key(index){return [...data.keys()][index]??null;},removeItem(key){data.delete(key);},setItem(key,value){data.set(key,value);} }; };
describe("fixed presentation and chronological source", () => {
  test("only the fixed root is the primary view, independent of container", () => { expect(isPrimaryActor("root")).toBe(true); expect(isPrimaryActor("project")).toBe(false); });
  test("queue previews use authorized resource addresses and expose account controls", () => {
    const queued = {...message,status:"queued" as const,attachments:[{id:"image-id",name:"preview.png",media_type:"image/png",size_bytes:42,uri:"https://untrusted.example/private"}]};
    const chat = {queueCursor:null} as unknown as Parameters<typeof MessageQueue>[0]["chat"];
    const html = renderToStaticMarkup(<TooltipProvider><MessageQueue messages={[queued]} active={message} chat={chat} access={{kind:"owner",manage:true}} /></TooltipProvider>);
    expect(html).toContain("/v1/actors/root/resources/image-id");expect(html).not.toContain("untrusted.example");
    expect(html).toContain("引导当前任务：");expect(html).toContain("撤回排队消息：");
  });
  test("primary IM excludes live model text, reasoning and tool pipeline", () => {
    const chat = {messages:[],tools:[],approvals:[],generations:{m:{assistant:"INTERNAL_STAGE",reasoning:"INTERNAL_THOUGHT"}},events:[],amend:async()=>{},refresh:async()=>{}} as unknown as Parameters<typeof PrimaryTurn>[0]["chat"];
    const html = renderToStaticMarkup(<PrimaryTurn message={message} chat={chat} access={{kind:"owner",manage:true}} onActor={()=>{}} />);
    expect(html).toContain("整理报告"); expect(html).not.toContain("INTERNAL_STAGE"); expect(html).not.toContain("INTERNAL_THOUGHT");
    const delivered = renderToStaticMarkup(<PrimaryTurn message={{...message,status:"completed",reply:"明确给用户的结果"}} chat={chat} access={{kind:"owner",manage:true}} onActor={()=>{}} />);
    expect(delivered).toContain("明确给用户的结果");
  });
  test("independent delivery renders immediately without a task activity round", () => {
    const chat = {messages:[],tools:[],approvals:[],events:[],toolNext:{}} as unknown as Parameters<typeof PrimaryTurn>[0]["chat"];
    const delivery:MessageRecord = {...message,id:"delivery",source:{kind:"actor",actor_id:"core"},kind:"delivery",in_reply_to:"m",content:"",status:"completed",reply:"任务已交给项目 Actor"};
    const primary=renderToStaticMarkup(<PrimaryTurn message={delivery} chat={chat} access={{kind:"owner",manage:true}} onActor={()=>{}}/>);
    expect(primary).toContain("任务已交给项目 Actor");expect(primary).not.toContain("dl-message-user");
    const work=renderToStaticMarkup(<WorkTurn message={delivery} chat={chat} access={{kind:"owner",manage:true}} onActor={()=>{}}/>);
    expect(work).toContain("任务已交给项目 Actor");expect(work).not.toContain("dl-work-round");
  });
  test("creating and delegating to an Actor gives one task link per request", () => {
    const tools = [
      {id:"create", message_id:"m", name:"actors__create", outcome:{actor_id:"child", name:"Project"}},
      {id:"send-1", message_id:"m", name:"actors__send", outcome:{actor_id:"child", message_id:"request-1"}},
      {id:"send-2", message_id:"m", name:"actors__send", outcome:{actor_id:"child", message_id:"request-2"}},
    ];
    const chat = {messages:[], tools, approvals:[], events:[]} as unknown as Parameters<typeof PrimaryTurn>[0]["chat"];
    const html = renderToStaticMarkup(<PrimaryTurn message={{...message,status:"completed"}} chat={chat} access={{kind:"owner",manage:true}} onActor={()=>{}} />);
    expect(html.match(/class="dl-actor-reference"/g)).toHaveLength(2);
  });
  test("prose interleaves with actual tool groups; final generation is not duplicated", () => {
    const records = [event(1,{type:"generation_started",message_id:"m",generation:1}),event(2,{type:"generation_delta",message_id:"m",generation:1,channel:"assistant",text:"先检查"}),event(3,{type:"tool_started",message_id:"m",call_id:"c",name:"files__read_file"}),event(4,{type:"generation_started",message_id:"m",generation:2}),event(5,{type:"generation_delta",message_id:"m",generation:2,channel:"assistant",text:"结果"})];
    expect(workTimeline(records,{...message,status:"completed",reply:"结果",final_generation:2}).map(item=>item.kind)).toEqual(["text","tools"]);
    expect(mergeActivity(records,[records[2]],"root","m")).toHaveLength(5);
    expect(workTimeline(records.slice(0,3),{...message,status:"completed",reply:"结果",final_generation:2},false)[0]).toMatchObject({content:"先检查"});
  });
  test("duration uses execution start, not queue creation; absent time stays absent", () => { expect(elapsedLabel(10_000,72_000)).toBe("1分钟 2秒"); expect(elapsedLabel(null,100)).toBeNull(); });
  test("completed activity begins collapsed under elapsed time without repeating intermediate text", () => {
    const completed = {...message,status:"completed" as const,started_at_ms:10_000,finished_at_ms:72_000,reply:"结果",final_generation:2};
    const html = renderToStaticMarkup(<WorkRound message={completed} events={[event(1,{type:"generation_delta",message_id:"m",generation:1,channel:"assistant",text:"INTERMEDIATE_TEXT"})]} tools={[]} onMoreTools={async()=>{}} onActor={()=>{}}/>);
    expect(html).toContain("用时 1分钟 2秒");
    expect(html).toContain('aria-expanded="false"');
    expect(html).not.toContain("INTERMEDIATE_TEXT");
  });
  test("a related reply or steering input does not remove the original task activity", () => {
    for (const kind of ["result", "steering"] as const) {
      const chat = {messages:[{...message,id:"related",kind,in_reply_to:message.id}],tools:[],approvals:[],events:[],toolNext:{}} as unknown as Parameters<typeof WorkTurn>[0]["chat"];
      const html = renderToStaticMarkup(<WorkTurn message={{...message,status:"completed",started_at_ms:10_000,finished_at_ms:72_000}} chat={chat} access={{kind:"owner",manage:true}} onActor={()=>{}} />);
      expect(html).toContain("dl-work-round");
      expect(html).toContain("用时 1分钟 2秒");
    }
  });
  test("removing the last provider keeps a save action; an unchanged empty setup has none", () => {
    const props: Parameters<typeof ProvidersSettings>[0] = {config:{providers:[],models:[],main_model:null,approval_model:null},onChange:()=>{},onSave:async()=>true,pending:false};
    const dirty = renderToStaticMarkup(<ProvidersSettings {...props} dirty />);
    expect(dirty).toContain("dl-settings-footer");
    expect(dirty.match(/<button[^>]*>保存<\/button>/)?.[0]).not.toContain('disabled=""');
    expect(dirty).not.toContain(i18n.t("providers_settings.model_required"));
    expect(renderToStaticMarkup(<ProvidersSettings {...props} />)).not.toContain("dl-settings-footer");
  });
  test("a working composer keeps stop alongside queue send, and root excludes execution selectors", () => {
    const props: Parameters<typeof Composer>[0] = {value:"queued draft",onValueChange:()=>{},onSubmit:()=>{},attachments:[],onFiles:()=>{},onRemoveAttachment:()=>{},uploading:false,busy:false,working:true,onStop:()=>{},canManage:true,mode:"ask",onModeChange:()=>{},models:[{name:"m",label:"fixture-model"}],modelName:"m",onModelChange:()=>{}};
    const root = renderToStaticMarkup(<TooltipProvider><Composer {...props} presentation="primary" /></TooltipProvider>);
    expect(root).toContain("停止当前任务"); expect(root).toContain("发送消息"); expect(root).not.toContain("加入队列"); expect(root).not.toContain("请求批准"); expect(root).not.toContain("fixture-model");
    const work = renderToStaticMarkup(<TooltipProvider><Composer {...props} presentation="work" /></TooltipProvider>);
    expect(work).toContain("加入队列"); expect(work).toContain("停止当前任务"); expect(work).toContain("请求批准"); expect(work).toContain("fixture-model");
  });
  test("sync readiness disables input, attachment controls and send while preserving the draft", () => {
    const status: CoreStatus = {worker_stage_id:"pc",epoch:1,synchronized:false,ready:false,model_ready:true,state:"synchronizing"};
    const render = (core: CoreStatus | undefined) => {
      const blocked = coreConnectionMessage(core);
      return renderToStaticMarkup(<TooltipProvider><Composer models={[]} modelName="" canManage={false} mode="auto" onModeChange={()=>{}} onModelChange={()=>{}} working={false} onStop={()=>{}} value="preserved draft" onValueChange={()=>{}} onSubmit={()=>{}} attachments={[]} onFiles={()=>{}} onRemoveAttachment={()=>{}} uploading={false} busy={false} disabled={Boolean(blocked)} placeholder={blocked ? i18n.t(blocked) : undefined} /></TooltipProvider>);
    };
    const before = render(status);
    expect(before).toContain("正在同步");
    expect(before).toMatch(/<textarea[^>]*disabled=""/);
    expect(before.match(/<button[^>]*aria-label="发送消息"[^>]*>/)?.[0]).toContain('disabled=""');
    expect(before.match(/<button[^>]*aria-label="添加附件"[^>]*>/)?.[0]).toContain('disabled=""');
    expect(before).toContain("preserved draft");
    const after = render({...status,synchronized:true,ready:true,state:"replica"});
    expect(after).not.toMatch(/<textarea[^>]*disabled=""/);
    expect(after).toContain("preserved draft");
    expect(render(undefined)).toMatch(/<textarea[^>]*disabled=""/);
    expect(coreConnectionMessage({...status,synchronized:true,state:"connecting"})).not.toBeNull();
  });
  test("actual diff carries both line numbers, with no invented change from arguments", () => {
    const html = renderToStaticMarkup(<FileChangeView path="source.ts" value={{added_lines:1,removed_lines:1,patch:"real patch",lines:[{kind:"remove",old_line:4,new_line:null,text:"old"},{kind:"add",old_line:null,new_line:4,text:"new"}]}} />);
    expect(html).toContain("dl-diff-line-added"); expect(html).toContain("dl-diff-line-removed"); expect(html).toContain("old"); expect(html).toContain("new");
    expect(renderToStaticMarkup(<FileChangeView path="source.ts" value={undefined} />)).toContain("未记录可视化文本差异");
  });
});
describe("durable drafts are identity/Actor partitioned data, never credentials", () => {
  test("refresh restores text, safe references and the pending operation key", () => {
    const store=memory(); saveDraft("owner:public-id:root","root",{text:"草稿",attachments:[{id:"opaque-id",name:"报告.txt",media_type:"text/plain",size_bytes:12,uri:"https://unsafe.example/file"}],pending:{content:"草稿",attachments:["opaque-id"],key:"operation-key"}},store,100);
    const recovered=loadDraft("owner:public-id:root","root",store,101)!;
    expect(recovered.text).toBe("草稿"); expect(recovered.attachments[0].uri).toBe("/v1/actors/root/resources/opaque-id"); expect(recovered.pending?.key).toBe("operation-key");
    expect(loadDraft("owner:another-id:root","root",store,101)).toBeUndefined(); expect(loadDraft("owner:public-id:root","different",store,101)).toBeUndefined();
  });
  test("expiry and logout remove private draft entries without touching preferences", () => { const store=memory();store.setItem("preferences","safe");saveDraft("owner:id:root","root",{text:"private",attachments:[]},store,100);expect(loadDraft("owner:id:root","root",store,100+13*3600_000)).toBeUndefined();saveDraft("owner:another:root","root",{text:"private",attachments:[]},store,100);clearStoredDrafts(store);expect(store.length).toBe(1);expect(store.getItem("preferences")).toBe("safe"); });
  test("malformed expiration cannot keep a private draft forever", () => { const store=memory();saveDraft("owner:id:root","root",{text:"private",attachments:[]},store,100);const key=store.key(0)!;const data=JSON.parse(store.getItem(key)!);data.expires_at="not a timestamp";store.setItem(key,JSON.stringify(data));expect(loadDraft("owner:id:root","root",store,101)).toBeUndefined();expect(store.length).toBe(0); });
});

beforeEach(async () => { await i18n.changeLanguage("zh-CN"); });
