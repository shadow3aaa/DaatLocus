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
const image = {uri:toolPreviewResource("screenshot"),name:"page-screenshot.svg",media_type:"image/svg+xml"};
const observation = {title:"Daat Locus · Browser preview",url:"https://example.com/",screenshot_url:image.uri};
const code = "export function greeting(name: string) {\n  return `Hello, ${name} 🌍`;\n}\n";
const change = {added_lines:2,removed_lines:1,lines:[
  {kind:"context",old_line:1,new_line:1,text:"export const layout = {"},
  {kind:"remove",old_line:2,new_line:null,text:"  gap: 24,"},
  {kind:"add",old_line:null,new_line:2,text:"  gap: 12,"},
  {kind:"add",old_line:null,new_line:3,text:"  preserveScroll: true,"},
  {kind:"context",old_line:3,new_line:4,text:"};"},
]};
const longChange = {added_lines:160,removed_lines:1,lines:Array.from({length:161},(_,i)=>({kind:i===0?"remove":"add",old_line:i===0?1:null,new_line:i===0?null:i,text:i%11===0?"":`export const row${i} = '${"Long source ".repeat(i%9===0?24:1)}';`}))};
const runningCommand = "Get-Content src/actors/store.rs | Select-Object -Skip 247 -First 54; Get-Content src/actors/types.rs -TotalCount 480";
const apps = [{name:"Files",description:"Files, skills and text search",enabled:true},{name:"Test MCP",description:"Third-party app",enabled:false}];
const mcp = "mcp_0123456789abcdef0123456789abcdef__inspect";
const permission = (decision: string, reason = "The current task is not authorized to write to this directory.") => ({approval:{decision,reason}});
// Fixed source descriptors stand in for Rust-derived result schemas in the mock transport.
const guiEntry={properties:{name:{},title:{},url:{format:"uri"},taken:{}}};
const guiSchema:JsonValue={properties:{status:{"x-dl-content":"status"},message:{},observation:{properties:{title:{},url:{format:"uri"}}},apps:{items:guiEntry},windows:{items:guiEntry},pages:{items:guiEntry},related_windows:{items:guiEntry}}};
const guiActions = (domain: "computer_use" | "phone_use", actions: string[]) => actions.map(action => sample(`${domain}-${action}`,`${domain === "computer_use" ? "Computer" : "Phone"} · ${action}`,`${domain}__${action}`,
  action === "list_apps" ? {apps:[{app_id:"notes",name:"Notes"},{app_id:"browser",name:"Browser"}]} : action === "list_windows" ? {windows:[{window_id:"notes-1",title:"Layout review · Notes",taken:true},{window_id:"browser-1",title:"Browser",taken:false}]} : {status:"observed",observation,resource_references:[image]},
  {schema:guiSchema,arguments:{window_id:"notes-1"}}));

/** Source-shaped examples for each retained renderer, including their exceptional branches. */
export const toolSections = [
  {id:"tools-files",label:"Tools · Files, skills and diffs",examples:[
    sample("file-read","Source code and line numbers","files__read_file",{path:"src/greeting.ts",content:code,start_line:12,end_line:14,total_lines:90},{content:"source",outcome:{path:"src/greeting.ts"}}),
    sample("file-read-empty","Empty file","files__read_file",{path:"empty.txt",content:"",start_line:1,end_line:1,total_lines:0},{content:"source"}),
    sample("file-skill","Read skill","files__read_skill",{path:"skills/browser/SKILL.md",content:"# Browser\n\nObserve before acting."},{content:"source"}),
    ...["write","edit","copy"].map(action=>sample(`file-${action}`,`${action} · Text diff`,`files__${action}_file`,{path:`src/${action}.ts`,change},{content:"file-change",arguments:{destination:`src/${action}.ts`},outcome:{path:`src/${action}.ts`,added_lines:2,removed_lines:1}})),
    sample("file-binary","No text diff for a binary file","files__copy_file",{path:"image.png"},{content:"file-change",outcome:{path:"image.png"}}),
    sample("file-diff-empty","No text changes","files__edit_file",{path:"same.txt",change:{added_lines:0,removed_lines:0,lines:[]}},{content:"file-change"}),
    sample("file-diff-long","Long lines, blank lines and lazy-loaded diffs","files__edit_file",{path:"src/long.ts",change:longChange},{content:"file-change",outcome:{path:"src/long.ts",added_lines:160,removed_lines:1}}),
    sample("file-directory","Directory and next-page cursor","files__list_directory",{path:"/workspace",entries:[{name:"src",kind:"directory"},{name:"README.md",kind:"file"}],next_cursor:"next-directory-page"}),
    sample("file-directory-empty","Empty directory","files__list_directory",{path:"/workspace/empty",entries:[],next_cursor:null}),
    sample("file-search","Multi-file search and result limits","files__search_text",{matches:[{path:"src/a.ts",line_number:24,text:"export const gap = 12;"},{path:"src/b.ts",line_number:8,text:"// Search result"}],limited:true},{arguments:{query:"gap"}}),
    sample("file-search-empty","No search matches","files__search_text",{matches:[],limited:false},{arguments:{query:"not_found"}}),
    sample("file-skills","Available skills","files__list_skills",{skills:[{name:"browser",description:"Observe and interact with the browser"},{name:"research",description:"Research and verify sources"}]}),
    sample("file-image","Image resources and deduplication","files__view_image",{resource_references:[image,image]}),
    sample("file-image-none","No image to preview","files__view_image",{resource_references:[]},{content:"image"}),
    sample("file-image-broken","Image loading failed","files__view_image",{resource_references:[{...image,uri:toolPreviewResource("missing-image")}]}),
  ]},
  {id:"tools-terminal",label:"Tools · Terminal and processes",examples:[
    sample("terminal-running","Running · Streaming output","terminal__terminal_exec",{session:{command:"bun test",status:"running",exit_code:null},captured_output:"Starting test suite…\n",running:true},{running:true,status:"running",content:"terminal",arguments:{command:runningCommand},outcome:{command:runningCommand}}),
    sample("terminal-controls","Polling parameters and model truncation stay out of logs","terminal__terminal_write_stdin",{mode:"poll",max_chars:6000,yield_time_ms:30000,wait_mode:"timeout",output:"[terminal output shortened by max_chars: 20302 char(s) omitted; showing tail]\nMODEL_ONLY",captured_output:"Compiling example v1.0.0\n    Finished dev profile in 3.2s\n",session:{command:"cargo build",status:"exited",exit_code:0}},{content:"terminal",outcome:{command:"cargo build"}}),
    sample("terminal-success","Successful command, stdout and stderr","terminal__terminal_exec",{command:"bun test",cwd:"/workspace",stdout:"204 pass\n0 fail\n",stderr:"warning: optional dependency unavailable",exit_code:0},{outcome:{command:"bun test"}}),
    sample("terminal-nonzero","Nonzero exit code and error output","terminal__terminal_exec",{command:"bun test",stdout:"1 test failed",stderr:"Expected 12, received 24",exit_code:1},{outcome:{command:"bun test"}}),
    sample("terminal-session","Session commands and continuous output","terminal__terminal_write_stdin",{session:{command:"vite --host 127.0.0.1",cwd:"/workspace",exit_code:null},output:"Local: http://127.0.0.1:5177/\nWaiting for changes…",running:true}),
    sample("terminal-long","Long logs and independent horizontal scrolling","terminal__terminal_exec",{command:"cargo test",stdout:Array.from({length:140},(_,i)=>`test ${i}: ${"output ".repeat(i%12===0?100:4)}`).join("\n"),exit_code:0}),
    sample("terminal-empty","Command with no output","terminal__terminal_exec",{command:"true",stdout:"",stderr:"",exit_code:0}),
    sample("terminal-stop","Process termination","terminal__terminal_terminate",{command:"vite",stdout:"Process terminated.",exit_code:143}),
    sample("terminal-error","Command failed to start","terminal__terminal_exec",{},{error:"Cannot start command: the working directory does not exist.",outcome:{command:"bun test"}}),
  ]},
  {id:"tools-web",label:"Tools · Web text and structured results",examples:[
    sample("web-text","Page text, links and pagination","web__run",{results:[{operation:"open",result:{title:"Example documentation",url:"https://example.com/docs",content:"# Documentation\n\nThis is the original text extracted from the page.\n"+"A long paragraph that should wrap naturally.".repeat(30),offset:0,next_offset:100,notices:["The page returns readable text only."]}}]}),
    sample("web-json","Structured JSON fields","web__run",{results:[{operation:"open",result:{media_type:"application/json",offset:0,next_offset:null,content:JSON.stringify({name:"Example API",ok:true,count:0,optional:null,items:[{name:"First item",value:42}]})}}]}),
    sample("web-json-invalid","Preserve malformed JSON as text","web__run",{results:[{operation:"open",result:{media_type:"application/json",offset:0,next_offset:null,content:'{"incomplete":'}}]}),
    sample("web-find","Find-in-page results","web__run",{results:[{operation:"find",result:{matches:[{content:"L12: Use Noto Sans."},{content:"L25: Use Maple Mono for code."}]}}]}),
    sample("web-find-empty","No find-in-page matches","web__run",{results:[{operation:"find",result:{matches:[]}}]}),
    sample("web-mixed","Success and failure in one group","web__run",{results:[{operation:"open",result:{title:"Accessible page",url:"https://example.com",content:"Read complete."}},{operation:"open",error:"The site returned 503. Please try again later."}]}),
    sample("web-empty","Empty response","web__run",{results:[]}),
    sample("web-close","Close page","web__close",{closed:true}),
  ]},
  {id:"tools-browser",label:"Tools · BrowserUse",examples:[
    sample("browser-list","Shared tabs","browser_use__list_pages",{pages:[{id:"docs",title:"Documentation",url:"https://example.com/docs"},{id:"search",title:"Search",url:"https://example.com/search"}]},{schema:guiSchema}),
    sample("browser-list-empty","No tabs","browser_use__list_pages",{pages:[]},{schema:guiSchema}),
    ...["open_page","observe","navigate","back","forward","reload","move_pointer","click","scroll","type_text","press_key","drag","close_page"].map(action=>sample(`browser-${action}`,`Browser · ${action}`,`browser_use__${action}`,{observation,resource_references:[image]},{schema:guiSchema,arguments:{page_id:"docs"},outcome:{observation:{title:observation.title}}})),
    sample("browser-unknown","Outcome unknown","browser_use__click",{status:"outcome_unknown",observation},{schema:guiSchema}),
    sample("browser-closed","Target tab closed","browser_use__click",{},{error:"The target tab has been closed."}),
  ]},
  {id:"tools-computer",label:"Tools · ComputerUse",examples:[
    ...guiActions("computer_use",["list_apps","list_windows","launch_app","open_window","get_window_state","adjust_window","click","type_text","press_key","scroll","drag","give_window","release_window","close_window"]),
    ...["dispatched","outcome_unknown","handoff_pending","given_to_user"].map(status=>sample(`computer-${status}`,`Computer action · ${status}`,"computer_use__click",{status,observation,related_windows:[{window_id:"child",title:"Save as",taken:false}]},{schema:guiSchema})),
    sample("computer-error","Desktop action failed with screenshot retained","computer_use__click",{observation,resource_references:[image]},{schema:guiSchema,error:"The target control is no longer available. Observe again."}),
  ]},
  {id:"tools-phone",label:"Tools · PhoneUse",examples:[
    ...guiActions("phone_use",["list_apps","list_windows","launch_app","open_window","get_window_state","click","set_text","press_key","scroll","drag","release_window","close_window"]),
    sample("phone-error","Phone connection failed","phone_use__get_window_state",{status:"outcome_unknown"},{error:"The device has disconnected."}),
  ]},
  {id:"tools-actors",label:"Tools · Actor collaboration",examples:[
    sample("actor-create","Create Actor","actors__create",{id:"atlas",name:"Atlas",conversation_url:"/actors/atlas"},{outcome:{actor_id:"atlas",name:"Atlas"}}),
    sample("actor-message","Send message","actors__message",{message_id:"layout-check",sent_message:{conversation_url:"/actors/atlas",content:"Review the **desktop layout**.\n\nKeep the `12px` spacing and check the [design document](https://example.com/design)."}},{outcome:{sent_message:true}}),
    sample("actor-report","Send progress","actors__reply",{message_id:"layout-report",sent_message:{conversation_url:"/actors/core",content:"Desktop checks are complete. Checking mobile wrapping."}},{outcome:{sent_message:true}}),
    sample("actor-list","Actor list and status","actors__list",{actors:[{id:"research",name:"Metis",status:"running"},{id:"atlas",name:"Atlas",status:"idle"}]}),
    sample("actor-history","History, nested fields and pagination","actors__read_history",{messages:[{name:"Daat Locus",content:"Please review the layout."},{name:"Metis",content:"Review complete."}],next_cursor:"30:0"}),
    sample("actor-resource-image","Read image resource","actors__read_resource",{resource_references:[image]}),
    sample("actor-resource-text","Full-output reference","actors__read_resource",{text:"This is the full attachment content."}),
    ...["yield","stop","archive","unarchive","import_conversation"].map(action=>sample(`actor-${action}`,`Collaboration · ${action}`,`${["stop","archive","unarchive"].includes(action)?"admin_actors":"actors"}__${action}`,{message:"Operation complete.",actor_id:"research"})),
    sample("actor-model","Change Actor model","admin_actors__set_model",{actor_id:"research",name:"Metis",model:"mock-mini"}),
    sample("actor-conversation","Read Actor bubble history","actors__read_conversation",{actor_id:"research",items:[{kind:"assistant",sender:"research",content:"Layout reviewed. Fixing the issues."}],next_cursor:null}),
    sample("actor-error","Sending to an archived Actor fails","actors__message",{},{error:"The target Actor is archived and cannot receive new messages."}),
  ]},
  {id:"tools-apps",label:"Tools · AppsManager",examples:[
    sample("apps-list","App list and enabled status","apps_manager__list",{apps}),
    sample("apps-empty","No apps available","apps_manager__list",{apps:[]}),
    ...["inspect","install","enable","close"].map(action=>sample(`apps-${action}`,`App · ${action}`,`apps_manager__${action}`,{app:{...apps[1],enabled:action!=="close"}})),
    sample("apps-refresh","Refresh apps","apps_manager__refresh",{changed:true}),
    sample("apps-error","App connection failed","apps_manager__install",{},{error:"MCP server failed to start: connection refused."}),
  ]},
  {id:"tools-mcp",label:"Tools · Standard MCP",examples:[
    sample("mcp-text","Multiple text blocks and links",mcp,{content:[{type:"text",text:"First block of tool output."},{type:"text",text:"Second block\nPreserve original line breaks."},{type:"resource_link",uri:"https://example.com/report",name:"Report"}]}),
    sample("mcp-structured","Structured results, resources and images",mcp,{content:[],structuredContent:{name:"Review report",ok:true,items:[{name:"Layout",score:100}],missing:null},resources:[{uri:toolPreviewResource("report"),name:"report.md"}],resource_references:[image]}),
    sample("mcp-unsafe-link","Ignore unsafe resource links",mcp,{content:[{type:"resource_link",uri:"javascript:alert(1)",name:"Unsafe link"},{type:"text",text:"Displayable text is retained."}]},{schema:{properties:{content:{items:{properties:{uri:{format:"uri"},text:{"x-dl-content":"text"},name:{}}}}}}}),
    sample("mcp-error","MCP failure with output retained",mcp,{content:[{type:"text",text:"Server response: query failed. Check the database connection."}]},{error:"MCP request failed"}),
    sample("mcp-empty","Empty MCP result",mcp,{content:[]}),
    sample("mcp-device","MCP call with a device prefix",`d_0123456789ab_${mcp}`,{content:[{type:"text",text:"The same tool on another device."}],structuredContent:{device:"Laptop",ready:true}}),
  ]},
  {id:"tools-artifacts",label:"Tools · Attachments and deliveries",examples:[
    ...["image","svg","html","file"].map(kind=>sample(`artifact-${kind}`,`Delivery · ${kind}`,"artifacts__present",{uri:toolPreviewResource(kind==="html"?"html":kind==="file"?"report":"screenshot"),title:`layout-report.${kind}`,kind,description:"Review results for the conversation and tool activity."},{content:"artifact",outcome:{uri:toolPreviewResource(kind==="html"?"html":kind==="file"?"report":"screenshot"),title:`layout-report.${kind}`,kind}})),
    sample("artifact-external","External result link","artifacts__present",{uri:"https://example.com/report",title:"External report",kind:"file"},{content:"artifact"}),
    sample("artifact-invalid","Invalid result link","artifacts__present",{uri:"javascript:alert(1)",title:"Result that cannot be opened"},{content:"artifact"}),
  ]},
  {id:"tools-permissions",label:"Tools · Permissions and Sudo",examples:[
    ...["denied","application_required","approved"].map(decision=>sample(`permission-${decision}`,`Permissions · ${decision}`,"files__write_file",permission(decision),{outcome:permission(decision)})),
    sample("sudo-wrapped","Unsandboxed · Terminal command","sudo__execute_as_admin",{sudo:{tool:"terminal__terminal_exec"},command:"repair-service",stdout:"Service repaired.",exit_code:0},{outcome:{operation:"terminal__terminal_exec",command:"repair-service"},arguments:{tool:"terminal__terminal_exec",arguments_json:JSON.stringify({command:"repair-service"}),reason:"Repair the service",restart_native:false,approval_id:null}}),
    sample("sudo-running","Unsandboxed · Running command","sudo__execute",{},{running:true,status:"running",outcome:{operation:"terminal__terminal_exec",command:runningCommand}}),
    sample("sudo-file","Unsandboxed · File diff","sudo__execute_as_admin",{path:"src/layout.ts",change,sudo:{tool:"files__edit_file",administrator:true}},{content:"file-change",outcome:{operation:"files__edit_file",path:"src/layout.ts",added_lines:2,removed_lines:1}}),
    sample("sudo-image","Unsandboxed · Original image preview","sudo__execute",{resource_references:[image],sudo:{tool:"files__view_image"}},{content:"image",outcome:{operation:"files__view_image"}}),
    sample("sudo-desktop","Unsandboxed · Computer window","sudo__execute_as_admin",{observation,resource_references:[image],sudo:{tool:"computer_use__get_window_state"}},{schema:guiSchema,outcome:{operation:"computer_use__get_window_state"}}),
    sample("sudo-submitted","Sudo submitted for review","sudo__execute",{status:"submitted",executed:false},{schema:{"x-dl-content":"status",properties:{status:{"x-dl-content":"status"},executed:{}}},outcome:{operation:"terminal__terminal_exec",command:"repair-service"}}),
    sample("sudo-invalid","Historical Sudo arguments cannot be unwrapped","sudo__execute",{sudo:{tool:"terminal__terminal_exec"},message:"Preserve the history."},{arguments:{arguments_json:"not valid JSON"}}),
    sample("sudo-nested","Prevent recursive Sudo unwrapping","sudo__execute",{sudo:{tool:"sudo__execute"},status:"submitted"}),
  ]},
  {id:"tools-fallback",label:"Tools · Generic structured output",examples:[
    sample("generic-text","Markdown and domain fields","extension__inspect",{message:"## Review complete\n\nPreserve **Markdown** and `inlineCode`.",path:"/workspace",count:2}),
    sample("generic-fields","Field types, nested collections and nulls","extension__inspect",{name:"Report",count:0,ready:false,missing:null,empty:"",items:[{name:"One",value:1},{title:"Two",value:2}],object:{path:"a.ts",status:"ok"},empty_items:[],empty_object:{},encoded:"{\"name\":\"Parsed domain data\",\"count\":2}",private_metadata:"PRIVATE_SENTINEL",model_transcript:"PRIVATE_SENTINEL"}),
    sample("generic-empty","Completion notice for empty results","extension__inspect",{}),
    sample("generic-device","Ordinary call with a device prefix","d_0123456789ab_files__read_file",{path:"remote.ts",content:code}),
  ]},
  {id:"tools-failures",label:"Tools · Failure, cancellation and read retry",examples:[
    sample("error-generic","Tool execution failed","extension__inspect",{},{error:"Request failed: the remote service is unavailable.",status:"failed"}),
    sample("error-long","Long error messages and line wrapping","files__read_file",{},{error:"Cannot read file:\n"+"/workspace/long-directory/".repeat(30)+"missing.ts\nTarget does not exist."}),
    sample("error-cancelled","Preserve completed effects after cancellation","terminal__terminal_exec",{command:"bun test",stdout:"Completed 3 checks before cancellation.",exit_code:130},{status:"cancelled"}),
    sample("error-interrupted","Preserve activity after interruption","files__read_file",{path:"src/a.ts",content:code},{status:"interrupted"}),
    sample("error-running","Tool still running","terminal__terminal_exec",{},{running:true,status:"running",arguments:{command:runningCommand},outcome:{command:runningCommand}}),
    sample("detail-loading","Loading expanded details","files__read_file",{path:"slow.ts",content:code},{network:"loading"}),
    sample("detail-retry","Detail loading fails, retry recovers","files__read_file",{path:"retry.ts",content:code},{network:"detail-error"}),
    sample("activity-retry","Activity loading fails, retry recovers","files__read_file",{path:"activity.ts",content:code},{network:"activity-error"}),
    sample("error-queued","Input not started yet","files__read_file",{},{status:"queued"}),
  ]},
] as const;
export const toolExamples: Example[] = toolSections.flatMap(section=>[...section.examples]);
export const toolScenes = [{id:"tools-all",label:`Tools · All rendering examples (${toolExamples.length} items)`,actor:"research"},...toolSections.map(section=>({id:section.id,label:section.label,actor:"research"}))] as const;
export function examplesForScene(scene: string): Example[] { return scene === "tools-all" ? toolExamples : [...toolSections.find(section=>section.id===scene)?.examples ?? []]; }
export function toolExampleResult(example: Example, actorId = "research"): ToolResult {
  return {id:`showcase-result-${example.id}`,actor_id:actorId,message_id:`showcase-message-${example.id}`,call_id:`showcase-call-${example.id}`,name:example.name,arguments:example.arguments??{},payload:example.payload,output_schema:example.schema??(example.content?{"x-dl-content":example.content}:undefined),outcome:example.outcome??{},summary:example.label,error:example.error??null,model_content:"PRIVATE_MODEL_TRANSCRIPT"};
}
