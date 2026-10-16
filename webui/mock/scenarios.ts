import type { CoreStatus, Device } from "../src/lib/stage-api";
import { resetStore } from "./fixtures";
import { addApproval, addMessage, addTool, configHolder, emitEvent, newId, now, ROOT_ACTOR_ID, setActorStatus, STAGE_ID, store, touchMessage, type ActorStore } from "./state";

export const scenarios = [
  {id:"conversation", label:"主会话", actor:ROOT_ACTOR_ID},
  {id:"empty", label:"空对话", actor:ROOT_ACTOR_ID},
  {id:"work", label:"工作对话", actor:"research"},
  {id:"loading", label:"消息加载中", actor:ROOT_ACTOR_ID},
  {id:"request-error", label:"读取失败与重试", actor:ROOT_ACTOR_ID},
  {id:"running", label:"正在执行与工具活动", actor:"research"},
  {id:"approval", label:"等待批准 · 长参数", actor:"research"},
  {id:"failure", label:"失败与重试", actor:"research"},
  {id:"queue", label:"排队与引导", actor:"research"},
  {id:"attachments", label:"图片与文件附件", actor:ROOT_ACTOR_ID},
  {id:"long-content", label:"长历史、代码、表格与差异", actor:"research"},
  {id:"many-approvals", label:"跨对话审批分页", actor:ROOT_ACTOR_ID},
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
  Object.assign(previewState,{id,authenticated:id!=="login"&&id!=="registration",registered:id!=="registration",devices:[],
    core:{worker_stage_id:STAGE_ID,epoch:1,synchronized:true,ready:true,model_ready:true,state:"replica",config_revision:"1"}});
  const core=store.actors.get(ROOT_ACTOR_ID)!;
  const work=store.actors.get("research")!;
  if(id==="approval"||id==="long-content") {
    work.summary.read_only=false;
    Object.assign(work.detail.config!.permissions,{read_only:false,writable_roots:["webui/src"]});
  }
  const clear=(actor:ActorStore)=>{actor.messages=[];actor.events=[];actor.tools=[];actor.approvals=[];actor.messageSeq=0;actor.eventSeq=0;actor.toolSeq=0;};
  if (id==="empty") clear(core);
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
      const approval=addApproval(work,{message_id:active.id,mode:"ask",call:{id:call,name:"files__edit_file",arguments:{path:"webui/src/components/conversation-pane.tsx",edits:Array.from({length:24},(_,i)=>({name:`布局修改 ${i+1}`,old_text:"padding-bottom: 24px;",new_text:"padding-bottom: calc(var(--dl-footer-space) + 24px);"}))}},requested:{readable_roots:["webui/src"],writable_roots:["webui/src"],operation_details:{path:"webui/src/components/conversation-pane.tsx"}}});
      emitEvent(work.summary.id,{type:"approval_requested",message_id:active.id,approval_id:approval.id,call_id:call,name:approval.call.name});
      work.detail.config!.permissions.approval_mode="ask";
      store.approvalWakers.set(approval.id,decision=>{
        if(store.actors.get(work.summary.id)!==work) return;
        const finished=now(), resultId=newId(), approved=decision==="approved";
        const reply=approved?"修改已完成，输入框下方的留白已经更新。":"这次修改未获批准，文件保持原样。";
        const error=approved?null:"approval_denied";
        addTool(work,{id:resultId,actor_id:work.summary.id,message_id:active.id,call_id:call,name:toolName,summary:reply,error,started_at_ms:active.started_at_ms!,finished_at_ms:finished,arguments:approval.call.arguments,model_content:reply,outcome:null,payload:null});
        emitEvent(work.summary.id,{type:"tool_finished",message_id:active.id,call_id:call,name:toolName,result_id:resultId,summary:reply,error});
        touchMessage(active,{status:"completed",reply,finished_at_ms:finished,final_generation:2});
        emitEvent(work.summary.id,{type:"message_updated",message_id:active.id,status:"completed"});
        setActorStatus(work,"idle");
      });
    }
    if(id==="failure") touchMessage(active,{status:"failed",reply:null,error:"provider_unavailable"});
    if(id==="queue") for(const content of ["再检查一下 320px 宽度。","把长代码和表格也加入检查。","记得保留输入框里的草稿。"]){const queued=addMessage(work,{kind:"request",source:{kind:"user"},reply_to:{kind:"user"},content,status:"queued"});emitEvent(work.summary.id,{type:"message_added",message_id:queued.id});}
  }
  if(id==="long-content") {
    for(let i=0;i<44;i++) message(work,`第 ${i+1} 项检查：消息区、标题和输入框在改变窗口尺寸时保持稳定。`);
    const record=message(work,"检查长代码、表格和文件差异的滚动条。" );
    touchMessage(record,{reply:"布局检查结果：\n\n```typescript\n"+Array.from({length:32},(_,i)=>`const viewport${i+1} = measureConversationLayout({ keepReadingPosition: true, reserveComposerSpace: true, preserveAttachmentDraft: true });`).join("\n")+"\n```\n\n| 位置 | 桌面 | 手机 | 历史位置 | 输入框展开 | 最后一条消息 |\n| --- | --- | --- | --- | --- | --- |\n"+Array.from({length:16},(_,i)=>`| 检查 ${i+1} | 正常 | 正常 | 保持 | 已检查 | 完整可见 |`).join("\n")});
    const toolId=newId(), callId=newId();
    addTool(work,{id:toolId,actor_id:work.summary.id,message_id:record.id,call_id:callId,name:"files__edit_file",summary:"调整对话区的底部留白",error:null,started_at_ms:now()-6000,finished_at_ms:now()-3000,arguments:{path:"webui/src/index.css"},model_content:"已更新对话区域留白。",outcome:{path:"webui/src/index.css"},payload:{path:"webui/src/index.css",change:{added_lines:24,removed_lines:24,lines:Array.from({length:48},(_,i)=>({kind:i%2?"add":"remove",old_line:i%2?null:i+1,new_line:i%2?i+1:null,text:i%2?"padding-bottom: calc(var(--dl-footer-space) + 24px);":"padding-bottom: 24px;"}))}}});
    emitEvent(work.summary.id,{type:"tool_finished",message_id:record.id,call_id:callId,name:"files__edit_file",result_id:toolId,summary:"调整对话区的底部留白",error:null});
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
  if(id==="many-approvals") for(let i=0;i<64;i++) {const record=message(work,`检查 ${i+1}` ,true);addApproval(work,{message_id:record.id,mode:"ask",call:{id:newId(),name:"files__read_file",arguments:{path:`webui/src/check-${i+1}.tsx`}},requested:{readable_roots:["webui/src"],writable_roots:[]}});}
  if(id==="devices") previewState.devices=[
    {stage_id:"laptop",label:"笔记本",connection:{status:"online",route:"lan",generation:1,retry_at_ms:0}},
    {stage_id:"phone",label:"手机",connection:{status:"connecting",route:null,generation:2,retry_at_ms:0}},
    {stage_id:"desktop",label:"台式机",error_code:"connection_failed",connection:{status:"offline",route:null,generation:1,retry_at_ms:0}},
  ];
  return true;
}
