import type { JsonValue, MessageStatus, ToolResult } from "../src/lib/actor-api";

export type ToolExample = {
  id: string; label: string; name: string; payload: JsonValue; arguments?: JsonValue; outcome?: JsonValue;
  error?: string; status?: MessageStatus; running?: boolean; network?: "loading" | "detail-error" | "activity-error";
  content?: "source" | "file-change" | "artifact" | "image" | "terminal";
  schema?:JsonValue;
};
type Example = ToolExample;
const sample = (id: string, label: string, name: string, payload: JsonValue, extra: Partial<Example> = {}): Example => ({id,label,name,payload,...extra});
export const toolPreviewResource = (name: string) => `/v1/actors/research/resources/showcase-${name}`;
const image = {uri:toolPreviewResource("screenshot"),name:"页面截图.svg",media_type:"image/svg+xml"};
const observation = {title:"Daat Locus · 浏览器预览",url:"https://example.com/",screenshot_url:image.uri};
const code = "export function greeting(name: string) {\n  return `你好，${name} 🌍`;\n}\n";
const change = {added_lines:2,removed_lines:1,lines:[
  {kind:"context",old_line:1,new_line:1,text:"export const layout = {"},
  {kind:"remove",old_line:2,new_line:null,text:"  gap: 24,"},
  {kind:"add",old_line:null,new_line:2,text:"  gap: 12,"},
  {kind:"add",old_line:null,new_line:3,text:"  preserveScroll: true,"},
  {kind:"context",old_line:3,new_line:4,text:"};"},
]};
const longChange = {added_lines:160,removed_lines:1,lines:Array.from({length:161},(_,i)=>({kind:i===0?"remove":"add",old_line:i===0?1:null,new_line:i===0?null:i,text:i%11===0?"":`export const row${i} = '${"很长的代码 / long source ".repeat(i%9===0?24:1)}';`}))};
const runningCommand = "Get-Content src/actors/store.rs | Select-Object -Skip 247 -First 54; Get-Content src/actors/types.rs -TotalCount 480";
const apps = [{name:"Files",description:"文件、技能与文本搜索",enabled:true},{name:"测试 MCP",description:"第三方应用",enabled:false}];
const mcp = "mcp_0123456789abcdef0123456789abcdef__inspect";
const permission = (decision: string, reason = "当前任务没有授权写入该目录。") => ({approval:{decision,reason}});
// Fixed source descriptors stand in for Rust-derived result schemas in the mock transport.
const guiEntry={properties:{name:{},title:{},url:{format:"uri"},taken:{}}};
const guiSchema:JsonValue={properties:{status:{"x-dl-content":"status"},message:{},observation:{properties:{title:{},url:{format:"uri"}}},apps:{items:guiEntry},windows:{items:guiEntry},pages:{items:guiEntry},related_windows:{items:guiEntry}}};
const guiActions = (domain: "computer_use" | "phone_use", actions: string[]) => actions.map(action => sample(`${domain}-${action}`,`${domain === "computer_use" ? "电脑" : "手机"} · ${action}`,`${domain}__${action}`,
  action === "list_apps" ? {apps:[{app_id:"notes",name:"备忘录"},{app_id:"browser",name:"浏览器"}]} : action === "list_windows" ? {windows:[{window_id:"notes-1",title:"布局检查 · 备忘录",taken:true},{window_id:"browser-1",title:"浏览器",taken:false}]} : {status:"observed",observation,resource_references:[image]},
  {schema:guiSchema,arguments:{window_id:"notes-1"}}));

/** Source-shaped examples for each retained renderer, including their exceptional branches. */
export const toolSections = [
  {id:"tools-files",label:"工具 · 文件、技能与差异",examples:[
    sample("file-read","读取源码与行号","files__read_file",{path:"src/greeting.ts",content:code,start_line:12,end_line:14,total_lines:90},{content:"source",outcome:{path:"src/greeting.ts"}}),
    sample("file-read-empty","空文件","files__read_file",{path:"empty.txt",content:"",start_line:1,end_line:1,total_lines:0},{content:"source"}),
    sample("file-skill","读取技能","files__read_skill",{path:"skills/browser/SKILL.md",content:"# 浏览器\n\n先观察，再操作。"},{content:"source"}),
    ...["write","edit","copy"].map(action=>sample(`file-${action}`,`${action} · 文本差异`,`files__${action}_file`,{path:`src/${action}.ts`,change},{content:"file-change",arguments:{destination:`src/${action}.ts`},outcome:{path:`src/${action}.ts`,added_lines:2,removed_lines:1}})),
    sample("file-binary","二进制文件无文本差异","files__copy_file",{path:"image.png"},{content:"file-change",outcome:{path:"image.png"}}),
    sample("file-diff-empty","没有文本更改","files__edit_file",{path:"same.txt",change:{added_lines:0,removed_lines:0,lines:[]}},{content:"file-change"}),
    sample("file-diff-long","超长行、空行与差异懒加载","files__edit_file",{path:"src/long.ts",change:longChange},{content:"file-change",outcome:{path:"src/long.ts",added_lines:160,removed_lines:1}}),
    sample("file-directory","目录与下一页标记","files__list_directory",{path:"/workspace",entries:[{name:"src",kind:"directory"},{name:"README.md",kind:"file"}],next_cursor:"next-directory-page"}),
    sample("file-directory-empty","空目录","files__list_directory",{path:"/workspace/empty",entries:[],next_cursor:null}),
    sample("file-search","多文件搜索与数量限制","files__search_text",{matches:[{path:"src/a.ts",line_number:24,text:"export const gap = 12;"},{path:"src/b.ts",line_number:8,text:"// 中文搜索结果"}],limited:true},{arguments:{query:"gap"}}),
    sample("file-search-empty","搜索无匹配","files__search_text",{matches:[],limited:false},{arguments:{query:"not_found"}}),
    sample("file-skills","可用技能列表","files__list_skills",{skills:[{name:"browser",description:"观察与操作浏览器"},{name:"research",description:"检索与核对资料"}]}),
    sample("file-image","图片资源与去重","files__view_image",{resource_references:[image,image]}),
    sample("file-image-none","没有可预览的图片","files__view_image",{resource_references:[]},{content:"image"}),
    sample("file-image-broken","图片加载失败","files__view_image",{resource_references:[{...image,uri:toolPreviewResource("missing-image")}]}),
  ]},
  {id:"tools-terminal",label:"工具 · 终端与进程",examples:[
    sample("terminal-running","正在运行 · 连续输出","terminal__terminal_exec",{session:{command:"bun test",status:"running",exit_code:null},captured_output:"Starting test suite…\n",running:true},{running:true,status:"running",content:"terminal",arguments:{command:runningCommand},outcome:{command:runningCommand}}),
    sample("terminal-controls","轮询参数与模型截断不进入日志","terminal__terminal_write_stdin",{mode:"poll",max_chars:6000,yield_time_ms:30000,wait_mode:"timeout",output:"[terminal output shortened by max_chars: 20302 char(s) omitted; showing tail]\nMODEL_ONLY",captured_output:"Compiling example v1.0.0\n    Finished dev profile in 3.2s\n",session:{command:"cargo build",status:"exited",exit_code:0}},{content:"terminal",outcome:{command:"cargo build"}}),
    sample("terminal-success","命令成功、stdout 与 stderr","terminal__terminal_exec",{command:"bun test",cwd:"/workspace",stdout:"204 pass\n0 fail\n",stderr:"warning: optional dependency unavailable",exit_code:0},{outcome:{command:"bun test"}}),
    sample("terminal-nonzero","非零退出码与错误输出","terminal__terminal_exec",{command:"bun test",stdout:"1 test failed",stderr:"Expected 12, received 24",exit_code:1},{outcome:{command:"bun test"}}),
    sample("terminal-session","会话命令与持续输出","terminal__terminal_write_stdin",{session:{command:"vite --host 127.0.0.1",cwd:"/workspace",exit_code:null},output:"Local: http://127.0.0.1:5177/\nWaiting for changes…",running:true}),
    sample("terminal-long","长日志与独立横向滚动","terminal__terminal_exec",{command:"cargo test",stdout:Array.from({length:140},(_,i)=>`test ${i}: ${"output ".repeat(i%12===0?100:4)}`).join("\n"),exit_code:0}),
    sample("terminal-empty","无输出命令","terminal__terminal_exec",{command:"true",stdout:"",stderr:"",exit_code:0}),
    sample("terminal-stop","进程终止","terminal__terminal_terminate",{command:"vite",stdout:"Process terminated.",exit_code:143}),
    sample("terminal-error","命令无法启动","terminal__terminal_exec",{},{error:"无法启动命令：工作目录不存在。",outcome:{command:"bun test"}}),
  ]},
  {id:"tools-web",label:"工具 · Web 文本与结构化结果",examples:[
    sample("web-text","网页文本、链接和分页","web__run",{results:[{operation:"open",result:{title:"示例文档",url:"https://example.com/docs",content:"# 文档\n\n这是网页提取的原始文本。\n"+"长段落，需要自然换行。".repeat(30),offset:0,next_offset:100,notices:["页面只返回可读取的文本内容。"]}}]}),
    sample("web-json","JSON 结构化字段","web__run",{results:[{operation:"open",result:{media_type:"application/json",offset:0,next_offset:null,content:JSON.stringify({name:"示例接口",ok:true,count:0,optional:null,items:[{name:"第一项",value:42}]})}}]}),
    sample("web-json-invalid","损坏的 JSON 保留原文","web__run",{results:[{operation:"open",result:{media_type:"application/json",offset:0,next_offset:null,content:'{"incomplete":'}}]}),
    sample("web-find","页面查找结果","web__run",{results:[{operation:"find",result:{matches:[{content:"L12: 使用 Noto Sans。"},{content:"L25: 代码使用 Maple Mono。"}]}}]}),
    sample("web-find-empty","页面查找无匹配","web__run",{results:[{operation:"find",result:{matches:[]}}]}),
    sample("web-mixed","同组成功与失败","web__run",{results:[{operation:"open",result:{title:"可访问的页面",url:"https://example.com",content:"读取完成。"}},{operation:"open",error:"目标网站返回 503，请稍后重试。"}]}),
    sample("web-empty","空响应","web__run",{results:[]}),
    sample("web-close","关闭页面","web__close",{closed:true}),
  ]},
  {id:"tools-browser",label:"工具 · BrowserUse",examples:[
    sample("browser-list","共享标签页列表","browser_use__list_pages",{pages:[{id:"docs",title:"文档",url:"https://example.com/docs"},{id:"search",title:"搜索",url:"https://example.com/search"}]},{schema:guiSchema}),
    sample("browser-list-empty","没有标签页","browser_use__list_pages",{pages:[]},{schema:guiSchema}),
    ...["open_page","observe","navigate","back","forward","reload","move_pointer","click","scroll","type_text","press_key","drag","close_page"].map(action=>sample(`browser-${action}`,`浏览器 · ${action}`,`browser_use__${action}`,{observation,resource_references:[image]},{schema:guiSchema,arguments:{page_id:"docs"},outcome:{observation:{title:observation.title}}})),
    sample("browser-unknown","操作结果未知","browser_use__click",{status:"outcome_unknown",observation},{schema:guiSchema}),
    sample("browser-closed","目标标签页已关闭","browser_use__click",{},{error:"目标标签页已经关闭。"}),
  ]},
  {id:"tools-computer",label:"工具 · ComputerUse",examples:[
    ...guiActions("computer_use",["list_apps","list_windows","launch_app","open_window","get_window_state","adjust_window","click","type_text","press_key","scroll","drag","give_window","release_window","close_window"]),
    ...["dispatched","outcome_unknown","handoff_pending","given_to_user"].map(status=>sample(`computer-${status}`,`电脑操作 · ${status}`,"computer_use__click",{status,observation,related_windows:[{window_id:"child",title:"另存为",taken:false}]},{schema:guiSchema})),
    sample("computer-error","桌面操作失败并保留截图","computer_use__click",{observation,resource_references:[image]},{schema:guiSchema,error:"目标控件已不可用，请重新观察。"}),
  ]},
  {id:"tools-phone",label:"工具 · PhoneUse",examples:[
    ...guiActions("phone_use",["list_apps","list_windows","launch_app","open_window","get_window_state","click","set_text","press_key","scroll","drag","release_window","close_window"]),
    sample("phone-error","手机连接失败","phone_use__get_window_state",{status:"outcome_unknown"},{error:"设备已断开连接。"}),
  ]},
  {id:"tools-actors",label:"工具 · Actor 协作",examples:[
    sample("actor-create","创建 Actor","actors__create",{id:"atlas",name:"Atlas",conversation_url:"/actors/atlas"},{outcome:{actor_id:"atlas",name:"Atlas"}}),
    sample("actor-message","发送消息","actors__message",{message_id:"layout-check",sent_message:{conversation_url:"/actors/atlas",content:"请检查 **桌面布局**。\n\n保留 `12px` 间距，并核对[设计文档](https://example.com/design)。"}},{outcome:{sent_message:true}}),
    sample("actor-report","发送进度","actors__reply",{message_id:"layout-report",sent_message:{conversation_url:"/actors/core",content:"桌面已检查完成，正在核对手机上的折行。"}},{outcome:{sent_message:true}}),
    sample("actor-list","Actor 列表与状态","actors__list",{actors:[{id:"research",name:"Iris",status:"running"},{id:"archived",name:"Atlas",status:"archived"},{id:"ready",name:"Argus",status:"idle"}]}),
    sample("actor-history","历史、嵌套字段与分页","actors__read_history",{messages:[{name:"Core",content:"请检查布局。"},{name:"Iris",content:"检查完成。"}],next_cursor:30}),
    sample("actor-resource-image","读取图片资源","actors__read_resource",{resource_references:[image]}),
    sample("actor-resource-text","读取完整输出提示","actors__read_resource",{text:"这是附件的完整内容。"}),
    ...["yield","stop","archive","unarchive","import_conversation"].map(action=>sample(`actor-${action}`,`协作 · ${action}`,`${["stop","archive","unarchive"].includes(action)?"admin_actors":"actors"}__${action}`,{message:"操作完成。",actor_id:"research"})),
    sample("actor-model","调整 Actor 模型","admin_actors__set_model",{actor_id:"research",name:"Iris",model:"economy"}),
    sample("actor-conversation","查看 Actor 气泡历史","admin_actors__read_conversation",{actor_id:"research",items:[{kind:"assistant",sender:"research",content:"布局已检查，正在修复。"}],next_cursor:null}),
    sample("actor-error","向已归档 Actor 发送失败","actors__message",{},{error:"目标 Actor 已归档，无法接收新消息。"}),
  ]},
  {id:"tools-apps",label:"工具 · AppsManager",examples:[
    sample("apps-list","应用列表与启用状态","apps_manager__list",{apps}),
    sample("apps-empty","没有可用应用","apps_manager__list",{apps:[]}),
    ...["inspect","install","enable","close"].map(action=>sample(`apps-${action}`,`应用 · ${action}`,`apps_manager__${action}`,{app:{...apps[1],enabled:action!=="close"}})),
    sample("apps-refresh","刷新应用","apps_manager__refresh",{changed:true}),
    sample("apps-error","应用连接失败","apps_manager__install",{},{error:"MCP 服务启动失败：连接被拒绝。"}),
  ]},
  {id:"tools-mcp",label:"工具 · 标准 MCP",examples:[
    sample("mcp-text","多段文本与链接",mcp,{content:[{type:"text",text:"第一段工具输出。"},{type:"text",text:"第二段\n保留原始换行。"},{type:"resource_link",uri:"https://example.com/report",name:"报告"}]}),
    sample("mcp-structured","结构化结果、资源与图片",mcp,{content:[],structuredContent:{name:"检查报告",ok:true,items:[{name:"布局",score:100}],missing:null},resources:[{uri:toolPreviewResource("report"),name:"report.md"}],resource_references:[image]}),
    sample("mcp-unsafe-link","忽略不安全资源链接",mcp,{content:[{type:"resource_link",uri:"javascript:alert(1)",name:"不安全链接"},{type:"text",text:"已保留可显示的文本。"}]},{schema:{properties:{content:{items:{properties:{uri:{format:"uri"},text:{"x-dl-content":"text"},name:{}}}}}}}),
    sample("mcp-error","MCP 失败且保留输出",mcp,{content:[{type:"text",text:"服务返回：查询失败，请检查数据库连接。"}]},{error:"MCP request failed"}),
    sample("mcp-empty","MCP 空结果",mcp,{content:[]}),
    sample("mcp-device","设备前缀下的 MCP 调用",`d_0123456789ab_${mcp}`,{content:[{type:"text",text:"来自其他设备的同一种工具。"}],structuredContent:{device:"笔记本",ready:true}}),
  ]},
  {id:"tools-artifacts",label:"工具 · 附件与交付结果",examples:[
    ...["image","svg","html","file"].map(kind=>sample(`artifact-${kind}`,`交付 · ${kind}`,"artifacts__present",{uri:toolPreviewResource(kind==="html"?"html":kind==="file"?"report":"screenshot"),title:`布局报告.${kind}`,kind,description:"消息区与工具活动的检查结果。"},{content:"artifact",outcome:{uri:toolPreviewResource(kind==="html"?"html":kind==="file"?"report":"screenshot"),title:`布局报告.${kind}`,kind}})),
    sample("artifact-external","外部结果链接","artifacts__present",{uri:"https://example.com/report",title:"外部报告",kind:"file"},{content:"artifact"}),
    sample("artifact-invalid","无效结果链接","artifacts__present",{uri:"javascript:alert(1)",title:"不可打开的结果"},{content:"artifact"}),
  ]},
  {id:"tools-permissions",label:"工具 · 权限与 Sudo",examples:[
    ...["denied","application_required","approved"].map(decision=>sample(`permission-${decision}`,`权限 · ${decision}`,"files__write_file",permission(decision),{outcome:permission(decision)})),
    sample("sudo-wrapped","无沙箱 · 终端命令","sudo__execute_as_admin",{sudo:{tool:"terminal__terminal_exec"},command:"repair-service",stdout:"Service repaired.",exit_code:0},{outcome:{operation:"terminal__terminal_exec",command:"repair-service"},arguments:{tool:"terminal__terminal_exec",arguments_json:JSON.stringify({command:"repair-service"}),reason:"Repair the service",restart_native:false,approval_id:null}}),
    sample("sudo-running","无沙箱 · 正在运行命令","sudo__execute",{},{running:true,status:"running",outcome:{operation:"terminal__terminal_exec",command:runningCommand}}),
    sample("sudo-file","无沙箱 · 文件差异","sudo__execute_as_admin",{path:"src/layout.ts",change,sudo:{tool:"files__edit_file",administrator:true}},{content:"file-change",outcome:{operation:"files__edit_file",path:"src/layout.ts",added_lines:2,removed_lines:1}}),
    sample("sudo-image","无沙箱 · 图片原样预览","sudo__execute",{resource_references:[image],sudo:{tool:"files__view_image"}},{content:"image",outcome:{operation:"files__view_image"}}),
    sample("sudo-desktop","无沙箱 · 电脑窗口","sudo__execute_as_admin",{observation,resource_references:[image],sudo:{tool:"computer_use__get_window_state"}},{schema:guiSchema,outcome:{operation:"computer_use__get_window_state"}}),
    sample("sudo-submitted","Sudo 已提交审核","sudo__execute",{status:"submitted",executed:false},{schema:{"x-dl-content":"status",properties:{status:{"x-dl-content":"status"},executed:{}}},outcome:{operation:"terminal__terminal_exec",command:"repair-service"}}),
    sample("sudo-invalid","历史 Sudo 参数无法解包","sudo__execute",{sudo:{tool:"terminal__terminal_exec"},message:"保留历史记录。"},{arguments:{arguments_json:"not valid JSON"}}),
    sample("sudo-nested","阻止递归 Sudo 解包","sudo__execute",{sudo:{tool:"sudo__execute"},status:"submitted"}),
  ]},
  {id:"tools-fallback",label:"工具 · 通用结构化输出",examples:[
    sample("generic-text","Markdown 与领域字段","extension__inspect",{message:"## 检查完成\n\n保留 **Markdown** 和 `inlineCode`。",path:"/workspace",count:2}),
    sample("generic-fields","字段类型、嵌套集合和空值","extension__inspect",{name:"报告",count:0,ready:false,missing:null,empty:"",items:[{name:"一",value:1},{title:"二",value:2}],object:{path:"a.ts",status:"ok"},empty_items:[],empty_object:{},encoded:'{"name":"已解析的领域数据","count":2}',private_metadata:"PRIVATE_SENTINEL",model_transcript:"PRIVATE_SENTINEL"}),
    sample("generic-empty","空结果的完成提示","extension__inspect",{}),
    sample("generic-device","设备前缀的普通调用","d_0123456789ab_files__read_file",{path:"remote.ts",content:code}),
  ]},
  {id:"tools-failures",label:"工具 · 失败、取消和读取重试",examples:[
    sample("error-generic","工具执行失败","extension__inspect",{},{error:"请求失败：远端服务不可用。",status:"failed"}),
    sample("error-long","很长的错误信息与换行","files__read_file",{},{error:"无法读取文件：\n"+"/workspace/很长的目录/".repeat(30)+"missing.ts\n目标不存在。"}),
    sample("error-cancelled","取消后保留已完成操作","terminal__terminal_exec",{command:"bun test",stdout:"Completed 3 checks before cancellation.",exit_code:130},{status:"cancelled"}),
    sample("error-interrupted","中断后保留活动","files__read_file",{path:"src/a.ts",content:code},{status:"interrupted"}),
    sample("error-running","工具仍在执行","terminal__terminal_exec",{},{running:true,status:"running",arguments:{command:runningCommand},outcome:{command:runningCommand}}),
    sample("detail-loading","展开详情时加载中","files__read_file",{path:"slow.ts",content:code},{network:"loading"}),
    sample("detail-retry","详情读取失败，重试恢复","files__read_file",{path:"retry.ts",content:code},{network:"detail-error"}),
    sample("activity-retry","活动记录读取失败，重试恢复","files__read_file",{path:"activity.ts",content:code},{network:"activity-error"}),
    sample("error-queued","尚未开始的输入","files__read_file",{},{status:"queued"}),
  ]},
] as const;
export const toolExamples: Example[] = toolSections.flatMap(section=>[...section.examples]);
export const toolScenes = [{id:"tools-all",label:`工具 · 全部渲染示例（${toolExamples.length} 项）`,actor:"research"},...toolSections.map(section=>({id:section.id,label:section.label,actor:"research"}))] as const;
export function examplesForScene(scene: string): Example[] { return scene === "tools-all" ? toolExamples : [...toolSections.find(section=>section.id===scene)?.examples ?? []]; }
export function toolExampleResult(example: Example, actorId = "research"): ToolResult {
  return {id:`showcase-result-${example.id}`,actor_id:actorId,message_id:`showcase-message-${example.id}`,call_id:`showcase-call-${example.id}`,name:example.name,arguments:example.arguments??{},payload:example.payload,output_schema:example.schema??(example.content?{"x-dl-content":example.content}:undefined),outcome:example.outcome??{},summary:example.label,error:example.error??null,model_content:"PRIVATE_MODEL_TRANSCRIPT"};
}
