import { resetApps } from "./apps";
import { resetInstructionLayers } from "./system-prompt";
import { advanceActivityPreview, seedActivityPreview, seedGuidanceActivityPreview, seedOverflowActivityPreview, seedPaginatedActivityPreview, seedProseActivityPreview } from "./activity";
import type { CoreStatus, Device } from "../src/lib/stage-api";
import { resetStore } from "./fixtures";
import { addApproval, addMessage, addTool, configHolder, emitEvent, newId, now, ROOT_ACTOR_ID, setActorStatus, STAGE_ID, store, touchMessage, type ActorStore } from "./state";

export const scenarios = [
  {id:"imports", label:"导入 · 本机会话选择", actor:ROOT_ACTOR_ID},
  {id:"imports-unsupported", label:"导入 · 不支持的设备隐藏入口", actor:ROOT_ACTOR_ID},
  {id:"import-running", label:"导入 · Core 正在准备 Actor", actor:ROOT_ACTOR_ID},
  {id:"import-failed", label:"导入 · 失败", actor:ROOT_ACTOR_ID},
  {id:"apps", label:"应用 · 列表与连接管理", actor:ROOT_ACTOR_ID},
  {id:"apps-empty", label:"应用 · 空列表", actor:ROOT_ACTOR_ID},
  {id:"apps-loading", label:"应用 · 加载中", actor:ROOT_ACTOR_ID},
  {id:"apps-error", label:"应用 · 加载失败", actor:ROOT_ACTOR_ID},
  {id:"conversation", label:"主会话", actor:ROOT_ACTOR_ID},
  {id:"bubble-spacing", label:"主对话 · 连续气泡与双方切换", actor:ROOT_ACTOR_ID},
  {id:"long-core", label:"主对话 · 长历史与长回复", actor:ROOT_ACTOR_ID},
  {id:"core-incoming", label:"主对话 · 处理中收到新消息", actor:ROOT_ACTOR_ID},
  {id:"empty", label:"空对话", actor:ROOT_ACTOR_ID},
  {id:"work", label:"工作对话", actor:"research"},
  {id:"activity-replay", label:"工作活动 · 分步演示", actor:"research"},
  {id:"activity-overflow", label:"工作活动 · 长时间执行与事件补载", actor:"research"},
  {id:"activity-guidance", label:"工作活动 · 执行中收到新消息", actor:"research"},
  {id:"activity-prose", label:"工作活动 · 正文与思考交错增量", actor:"research"},
  {id:"activity-thinking", label:"工作活动 · 思考中的连续工具组", actor:"research"},
  {id:"activity-tools", label:"工作活动 · 工具执行中", actor:"research"},
  {id:"activity-complete", label:"工作活动 · 完成与正文交错", actor:"research"},
  {id:"activity-pagination", label:"工作活动 · 长记录滚动加载", actor:"research"},
  {id:"thinking-only", label:"工作活动 · 尚未调用工具", actor:"research"},
  {id:"loading", label:"消息加载中", actor:ROOT_ACTOR_ID},
  {id:"request-error", label:"读取失败与重试", actor:ROOT_ACTOR_ID},
  {id:"running", label:"正在执行与工具活动", actor:"research"},
  {id:"approval", label:"范围申请 · Core 确认", actor:"research"},
  {id:"failure", label:"失败与重试", actor:"research"},
  {id:"queue", label:"处理中收到新消息", actor:"research"},
  {id:"attachments", label:"图片与文件附件", actor:ROOT_ACTOR_ID},
  {id:"long-content", label:"长历史、代码、表格与差异", actor:"research"},
  {id:"diff-overflow", label:"文件差异 · 超长行、空行与滚动补载", actor:"research"},
  {id:"history", label:"历史刻度与悬停预览", actor:"research"},
  {id:"many-approvals", label:"范围申请历史分页", actor:ROOT_ACTOR_ID},
  {id:"connecting", label:"正在连接", actor:ROOT_ACTOR_ID},
  {id:"reconnecting", label:"事件流断线重连", actor:ROOT_ACTOR_ID},
  {id:"synchronizing", label:"同步中", actor:ROOT_ACTOR_ID},
  {id:"recovering", label:"恢复中", actor:ROOT_ACTOR_ID},
  {id:"reconciling", label:"合并中", actor:ROOT_ACTOR_ID},
  {id:"offline", label:"离线", actor:ROOT_ACTOR_ID},
  {id:"recovery_blocked", label:"恢复受阻", actor:ROOT_ACTOR_ID},
  {id:"models-empty", label:"没有模型", actor:ROOT_ACTOR_ID},
  {id:"devices", label:"设备连接状态", actor:ROOT_ACTOR_ID},
  {id:"login", label:"账号登录", actor:ROOT_ACTOR_ID},
  {id:"registration", label:"创建账号入口", actor:ROOT_ACTOR_ID},
] as const;
export type ScenarioId = typeof scenarios[number]["id"];
export const previewState: {id:ScenarioId; authenticated:boolean; registered:boolean; core:CoreStatus; devices:Device[]} = {
  id:"conversation", authenticated:true, registered:true,
  core:{worker_stage_id:STAGE_ID, epoch:1, synchronized:true, ready:true, model_ready:true, state:"replica", config_revision:"1"}, devices:[],
};

function message(actor: ActorStore, content: string, running = false) {
  const record = addMessage(actor,{kind:"request",source:{kind:"user"},reply_to:{kind:"user"},content,status:running?"running":"completed"});
  touchMessage(record,{started_at_ms:now()-65000,finished_at_ms:running?null:now()-3000,reply:running?null:"检查完成。最后一条消息能完整显示在输入框上方。",final_generation:2});
  emitEvent(actor.summary.id,{type:"message_added",message_id:record.id});
  emitEvent(actor.summary.id,{type:"generation_started",message_id:record.id,generation:1});
  emitEvent(actor.summary.id,{type:"generation_delta",message_id:record.id,generation:1,channel:"assistant",text:"我先检查消息区的滚动范围，再查看输入框展开后的布局。"});
  return record;
}

export function activateScenario(id: string): boolean {
  if (!scenarios.some(scene=>scene.id===id)) return false;
  resetStore();
  resetApps(id === "apps-empty");
  resetInstructionLayers();
  Object.assign(previewState,{id,authenticated:id!=="login"&&id!=="registration",registered:id!=="registration",devices:[],
    core:{worker_stage_id:STAGE_ID,epoch:1,synchronized:true,ready:true,model_ready:true,state:"replica",config_revision:"1"}});
  const core=store.actors.get(ROOT_ACTOR_ID)!;
  const work=store.actors.get("research")!;
  if(id==="approval"||id==="long-content") {
  }
  const clear=(actor:ActorStore)=>{actor.messages=[];actor.events=[];actor.tools=[];actor.approvals=[];actor.messageSeq=0;actor.eventSeq=0;actor.toolSeq=0;};
  if (id.startsWith("activity-") || id === "thinking-only") {
    clear(work);
    if (id === "activity-pagination") seedPaginatedActivityPreview(work);
    else if (id === "activity-overflow") seedOverflowActivityPreview(work);
    else if (id === "activity-guidance") seedGuidanceActivityPreview(work);
    else if (id === "activity-prose") seedProseActivityPreview(work);
    else seedActivityPreview(work, id === "thinking-only");
    if (id === "activity-tools") advanceActivityPreview();
    if (id === "activity-complete") while (advanceActivityPreview()) { /* Seed all settled phases. */ }
  }
  if (id.startsWith("import-")) {
    clear(core);
    const record = addMessage(core, {kind:"request",source:{kind:"user"},reply_to:{kind:"user"},content:"Import the attached Codex conversation; prepare an idle Actor without sending work.",status:id==="import-running"?"running":"failed",attachments:[{id:"import-example",name:"检查消息滚动与输入框布局",media_type:"application/vnd.daat-locus.codex-conversation+json",size_bytes:640,uri:"/v1/actors/core/resources/import-example"}]});
    if(id==="import-failed")touchMessage(record,{error:"Model provider is unavailable",finished_at_ms:now()});
    emitEvent(core.summary.id,{type:"message_added",message_id:record.id});
  }
  if (id==="empty") clear(core);
  if (id === "core-incoming") {
    clear(core);
    message(core, "请检查这份界面的布局。", true);
    addMessage(core, {kind:"request",source:{kind:"user"},reply_to:{kind:"user"},content:"再补充一点：也检查一下手机上的显示。",status:"queued"});
    addMessage(core, {kind:"request",source:{kind:"user"},reply_to:{kind:"user"},content:"保留正在输入的草稿。",status:"queued"});
    setActorStatus(core, "running");
  }
  if (id === "bubble-spacing") {
    clear(core);
    const request = message(core, "重新发我看看。");
    touchMessage(request, { reply: "这是之前修正过的初稿，先重新发你看看。我接着核对要求和还需补齐的部分。" });
    const deliver = (reply: string) => {
      const record = addMessage(core, { kind: "delivery", source: {kind: "actor", actor_id: ROOT_ACTOR_ID}, reply_to: {kind: "user"}, content: "", status: "completed" });
      touchMessage(record, {reply, finished_at_ms: now()});
      return record;
    };
    // An internal record between deliveries must not create an empty visual turn.
    deliver("");
    deliver("核对完了，18 页排版和四张图都正常，图 3 的修正也在。\n\n这份可以直接审阅。你看时重点检查第 17 页的待确认问题，以及第 12 页的性能目标是否符合你的设想。");
    const followUp = message(core, "这些模板部分来自模板要求吗？");
    touchMessage(followUp, {reply: "我核对一下这两项的来源，区分老师明确要求的内容和我整理文档时加的结构。"});
    const report = deliver("按我读取指导书后的记录，这两类内容有要求依据，但具体表格是我整理的：\n\n- 需求提出：记录需求编号、提出人、日期与原始描述。\n- 修订记录：记录评审后改了什么，以及待确定问题清单。\n\n表格样式和栏目是否必须原样保留，还需核对原文。");
    addTool(core, {id: newId(), actor_id: ROOT_ACTOR_ID, message_id: report.id, call_id: newId(), name: "actors__message", summary: "核对原文", error: null, started_at_ms: now()-1000, finished_at_ms: now(), arguments: {}, model_content: "已交给研究员", outcome: {actor_id: "research", name: "研究员", show_to_user: false}, payload: null});
    deliver("我也会检查原文，确认哪些必须保留，哪些可以删简。");
    addMessage(core, {kind: "request", source: {kind: "user"}, reply_to: {kind: "user"}, content: "好，再看看原文。", status: "queued"});
    addMessage(core, {kind: "request", source: {kind: "user"}, reply_to: {kind: "user"}, content: "保留需要确认的地方。", status: "queued"});
  }
  if (["connecting","synchronizing","recovering","reconciling","offline","recovery_blocked"].includes(id)) {
    Object.assign(previewState.core,{state:id,ready:false,synchronized:false});
  }
  if (id==="models-empty") {configHolder.current={providers:[],models:[],main_model:null,approval_model:null};Object.assign(previewState.core,{state:"model_required",model_ready:false});}
  if (["running","approval","failure","queue"].includes(id)) {
    const active=message(work,"检查对话界面的滚动行为，保留输入框中的草稿。",id!=="failure");
    const call=newId();
    const toolName=id==="approval"?"files__edit_file":"files__read_file";
    emitEvent(work.summary.id,{type:"tool_started",message_id:active.id,call_id:call,name:toolName});
    if(id!=="failure") setActorStatus(work,"running");
    if(id==="approval") {
      const approval=addApproval(work,{message_id:active.id,call:{id:call,name:"files__edit_file",arguments:{path:"/reports/layout.tsx",old_text:"padding-bottom: 24px;",new_text:"padding-bottom: 32px;"}},requested:{readable_roots:[],writable_roots:["/reports"]}});
      approval.decision="denied";
      approval.reason="The requested directory is outside the current sandbox scope.";
      approval.resolved_at_ms=now();
      emitEvent(work.summary.id,{type:"approval_resolved",message_id:active.id,approval_id:approval.id,decision:"denied",reason:approval.reason});
      const reply="需要额外的目录写入范围，已将申请提交给 Core。";
      touchMessage(active,{status:"completed",reply,finished_at_ms:now(),final_generation:2});
      emitEvent(work.summary.id,{type:"message_updated",message_id:active.id,status:"completed"});
      setActorStatus(work,"idle");
      const request=message(core,"请生成布局报告。" );
      touchMessage(request,{reply:"生成报告需要写入 /reports 目录，是否同意？"});
    }
    if(id==="failure") touchMessage(active,{status:"failed",reply:null,error:"provider_unavailable"});
    if(id==="queue") for(const content of ["再检查一下 320px 宽度。","把长代码和表格也加入检查。","记得保留输入框里的草稿。"]){const queued=addMessage(work,{kind:"request",source:{kind:"user"},reply_to:{kind:"user"},content,status:"queued"});emitEvent(work.summary.id,{type:"message_added",message_id:queued.id});}
  }
  if(id==="long-core") {
    for(let i=0;i<36;i++) {
      const record=message(core,`检查记录 ${i+1}：请继续检查桌面与手机的消息布局。`);
      touchMessage(record,{reply:`第 ${i+1} 项已检查，消息区和输入框的位置保持一致。`});
    }
    const record=message(core,"把本次检查的结论整理一下。");
    touchMessage(record,{reply:"已完成界面检查，结果如下。\n\n"+Array.from({length:6},(_,i)=>`### ${i+1}. ${["窗口与边界","消息阅读","长内容","输入框","页面导航","窄屏布局"][i]}\n\n消息在主区域内自然滚动，标题和输入框悬浮在其上方。较长的段落会随窗口宽度换行，最后一段内容仍能完整读到。\n\n- 检查了桌面和手机布局。\n- 保留正在阅读的位置和未发送的草稿。`).join("\n\n")+"\n\n所有检查已完成。"});
  }
  if(id==="history") {
    clear(work);
    for(let i=0;i<49;i++) {
      const record=message(work,`第 ${i+1} 轮：${["检查消息滚动时的位置。","看看输入框展开后的留白。","检查窄屏换行和返回按钮。"][i%3]}`);
      touchMessage(record,{reply:i%3===0 ? "已检查这部分布局。\n\n- 消息保持原有阅读位置。\n- 输入框展开后仍能读到末尾。\n- 手机和桌面使用相同的消息内容。" : "这部分已经检查完成，窗口变化时可以继续阅读。"});
    }
    const record=message(work,"把这轮界面检查的结果整理一下。");
    touchMessage(record,{reply:"这轮布局检查已完成。\n\n- 保留历史消息的阅读位置。\n- 输入框展开时，末尾仍然完整可见。\n- 窄屏使用全屏对话和返回按钮。\n\n还检查了长段落、列表和代码的换行。"});
    // The task started outside the newest page. Opening its card should still
    // enter at the newest edge; only an explicit search should fetch that input.
    const task = core.tools.find(tool => tool.result.name === "actors__message");
    if (task) task.result.outcome = {actor_id:"research",name:"检查消息滚动与输入框布局",message_id:work.messages[0].id};
  }
  if(id==="long-content") {
    for(let i=0;i<44;i++) message(work,`第 ${i+1} 项检查：消息区、标题和输入框在改变窗口尺寸时保持稳定。`);
    const record=message(work,"检查长代码、表格和文件差异的滚动条。" );
    touchMessage(record,{reply:"布局检查结果：\n\n```typescript\n"+Array.from({length:32},(_,i)=>`const viewport${i+1} = measureConversationLayout({ keepReadingPosition: true, reserveComposerSpace: true, preserveAttachmentDraft: true });`).join("\n")+"\n```\n\n| 位置 | 桌面 | 手机 | 历史位置 | 输入框展开 | 最后一条消息 |\n| --- | --- | --- | --- | --- | --- |\n"+Array.from({length:16},(_,i)=>`| 检查 ${i+1} | 正常 | 正常 | 保持 | 已检查 | 完整可见 |`).join("\n")});
    const toolId=newId(), callId=newId();
    addTool(work,{id:toolId,actor_id:work.summary.id,message_id:record.id,call_id:callId,name:"files__edit_file",summary:"调整对话区的底部留白",error:null,started_at_ms:now()-6000,finished_at_ms:now()-3000,arguments:{path:"webui/src/index.css"},model_content:"已更新对话区域留白。",outcome:{path:"webui/src/index.css"},payload:{path:"webui/src/index.css",change:{added_lines:24,removed_lines:24,lines:Array.from({length:48},(_,i)=>({kind:i%2?"add":"remove",old_line:i%2?null:i+1,new_line:i%2?i+1:null,text:i%2?"padding-bottom: calc(var(--dl-footer-space) + 24px);":"padding-bottom: 24px;"}))}}});
    emitEvent(work.summary.id,{type:"tool_finished",message_id:record.id,call_id:callId,name:"files__edit_file",result_id:toolId,summary:"调整对话区的底部留白",error:null});
  }
  if(id==="diff-overflow") {
    clear(work);
    const record=message(work,"检查长 Markdown 行和多文件差异。" );
    touchMessage(record,{reply:"已更新变更记录和相关文件。"});
    const path="C:\\Users\\Example\\project\\docs\\LONG_ALIGNMENT_CHANGELOG.local.md";
    for (let version=0;version<7;version++) {
      const filename=version<2?path:`src/components/layout-${version}.tsx`;
      const lines=Array.from({length:version===0?185:12},(_,index)=>{
        const kind=index%6===2?"remove":index%6===3||index%6===0?"add":"context";
        const text=index%6===1?"":index===0?"# Long Markdown verification — "+"保留完整行背景、行号和原始间距。 ".repeat(32):index%6===2?"const result = previousLayout({ padding: 24, selected: false });":index%6===3?"const result = currentLayout({ padding: 32, selected: true });":`Line ${index+1}: preserve source whitespace and <literal> text.\t${"Long source content without wrapping. ".repeat(index%8===4?25:1)}`;
        return {kind,text,old_line:kind==="add"?null:99000+index,new_line:kind==="remove"?null:99000+index};
      });
      const added=lines.filter(line=>line.kind==="add").length, removed=lines.filter(line=>line.kind==="remove").length;
      const toolId=newId(),callId=newId();
      addTool(work,{id:toolId,actor_id:work.summary.id,message_id:record.id,call_id:callId,name:"files__edit_file",summary:"更新文件",error:null,started_at_ms:now()-6000,finished_at_ms:now()-3000,arguments:{path:filename},model_content:"文件已更新。",outcome:{path:filename,added_lines:added,removed_lines:removed},payload:{path:filename,change:{added_lines:added,removed_lines:removed,lines}}});
      emitEvent(work.summary.id,{type:"tool_finished",message_id:record.id,call_id:callId,name:"files__edit_file",result_id:toolId,summary:"更新文件",error:null});
    }
  }
  if(id==="attachments") {
    const bytes=new TextEncoder().encode("# 布局检查\n\n消息区铺满容器，输入框悬浮在底部。\n");
    const attachment={id:"layout-note",name:"布局检查.md",media_type:"text/markdown",size_bytes:bytes.length,uri:`/v1/actors/${ROOT_ACTOR_ID}/resources/layout-note`};
    store.attachments.set(attachment.id,{attachment,bytes,media_type:attachment.media_type});
    const imageBytes=new TextEncoder().encode('<svg xmlns="http://www.w3.org/2000/svg" width="640" height="360" viewBox="0 0 640 360"><rect width="640" height="360" rx="24" fill="#eff2f6"/><rect x="32" y="32" width="576" height="296" rx="20" fill="white"/><rect x="340" y="70" width="230" height="48" rx="24" fill="#3566f0"/><rect x="70" y="144" width="370" height="88" rx="22" fill="#f0f0f0"/><rect x="70" y="264" width="500" height="40" rx="20" fill="#f7f7f7"/></svg>');
    const image={id:"layout-image",name:"对话布局.svg",media_type:"image/svg+xml",size_bytes:imageBytes.length,uri:`/v1/actors/${ROOT_ACTOR_ID}/resources/layout-image`};
    store.attachments.set(image.id,{attachment:image,bytes:imageBytes,media_type:image.media_type});
    const record=message(core,"帮我看看这份布局检查记录。" );record.attachments=[image,attachment];
  }
  if(id==="many-approvals") for(let i=0;i<64;i++) {const record=message(work,`检查 ${i+1}` ,true);addApproval(work,{message_id:record.id,call:{id:newId(),name:"files__read_file",arguments:{path:`webui/src/check-${i+1}.tsx`}},requested:{readable_roots:["webui/src"],writable_roots:[]}});}
  if(id==="devices") previewState.devices=[
    {stage_id:"laptop",label:"笔记本",connection:{status:"online",route:"lan",generation:1,retry_at_ms:0}},
    {stage_id:"phone",label:"手机",connection:{status:"connecting",route:null,generation:2,retry_at_ms:0}},
    {stage_id:"desktop",label:"台式机",error_code:"connection_failed",connection:{status:"offline",route:null,generation:1,retry_at_ms:0}},
  ];
  return true;
}
