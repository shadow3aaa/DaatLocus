import i18n from "../src/lib/i18n";
import { describe, expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { ToolDetail } from "../src/components/tool-details";
import { ToolParameters } from "../src/components/tool-parameters";
import { MessageOutcomes, messageOutcomeTools } from "../src/components/message-outcomes";
import { readableSummary, activityLabel, approvalNotice, operationToolName, toolIcon } from "../src/lib/tool-presentation";
import { Activities } from "../src/components/conversation-content";
import { toolResultTitle } from "../src/lib/tool-content";
import type { ToolResult, JsonValue } from "../src/lib/actor-api";

const sourceSchema={"x-dl-content":"source"};
const changeSchema={"x-dl-content":"file-change"};
const imageSchema={"x-dl-content":"image"};
const guiSchema:JsonValue={properties:{status:{},observation:{properties:{title:{},url:{format:"uri"}}},windows:{items:{properties:{title:{}}}},apps:{items:{properties:{name:{}}}}}};
const mcpSchema:JsonValue={"x-dl-error":"generic",properties:{content:{items:{properties:{text:{"x-dl-content":"text"},name:{},uri:{format:"uri"}}}},structuredContent:{},resources:{items:{properties:{name:{},uri:{format:"uri"}}}}}};
const webSchema:JsonValue={properties:{results:{items:{properties:{operation:{},error:{},result:{properties:{title:{},url:{format:"uri"},content:{},structured_content:{},next_offset:{}}}}}}}};

const result = (patch: Partial<ToolResult>): ToolResult => ({ id: "private-result-id", actor_id: "root", message_id: "message", call_id: "call", name: "extension__inspect", arguments: {}, payload: { private_metadata: "HIDDEN_PAYLOAD" }, model_content: '{"internal":"HIDDEN_MODEL_PROTOCOL"}', summary: "检查已完成", error: null, ...patch });

describe("tool observations are product content, never protocol dumps", () => {
  test("Rust output descriptors drive identical content across arbitrary tool names",()=>{
    const payload={path:"example.ts",content:"const answer = 42;",start_line:1,end_line:1,total_lines:1};
    const render=(name:string)=>renderToStaticMarkup(<ToolDetail result={result({name,payload,output_schema:sourceSchema})}/>);
    expect(render("extension__source")).toBe(render("d_abcdef123456_custom__another_operation"));
    expect(render("extension__source")).toContain("const answer = 42;");
  });
  test("new struct fields, nullable definitions and collections need no tool-specific client code",()=>{
    const schema:JsonValue={$defs:{Entry:{type:"object",properties:{future_field:{title:"New field"},enabled:{}}}},properties:{items:{anyOf:[{type:"array",items:{$ref:"#/$defs/Entry"}},{type:"null"}]}}};
    const html=renderToStaticMarkup(<ToolDetail result={result({output_schema:schema,payload:{items:[{future_field:"SERDE_VALUE",enabled:false,private_driver:"HIDDEN_NATIVE"}]}})}/>);
    expect(html).toContain("New field");expect(html).toContain("SERDE_VALUE");expect(html).toContain(i18n.t("tool_parameters.no"));expect(html).not.toContain("HIDDEN_NATIVE");
  });
  test("generic collections render a bounded first page and continue through the shared lazy boundary",()=>{
    const html=renderToStaticMarkup(<ToolDetail result={result({output_schema:{properties:{entries:{items:{properties:{title:{}}}}}},payload:{entries:Array.from({length:100},(_,index)=>({title:`ENTRY_${index}_END`}))}})}/>);
    expect(html).toContain("ENTRY_29_END");expect(html).not.toContain("ENTRY_30_END");
    expect(html).toContain("data-lazy-page");expect(html).not.toContain("<button");
    expect(html).toContain("dl-observation-collection");
  });
  test("collapsed running activity previews the actual command instead of a fixed group title",()=>{
    const command="Get-Content src/actors/store.rs | Select-Object -Skip 247 -First 54";
    const tool={seq:1,...result({id:"live:terminal",name:"terminal__terminal_exec",outcome:{command}}),status:"running" as const};
    const html=renderToStaticMarkup(<Activities actorId="root" tools={[tool]} onActor={()=>{}}/>);
    expect(html).toContain(`data-running-command="true"`);
    expect(html).toContain(`${i18n.t("tool_presentation.running")} ${command}`);
    expect(html).not.toContain(i18n.t("tool_presentation.ran_commands"));
    expect(html).toContain('aria-expanded="false"');
  });
  test("known operations never echo their arbitrary summary as a second operation title",()=>{
    for(const name of ["computer_use__list_apps","phone_use__list_apps","files__list_directory"]){const label=activityLabel({seq:1,...result({name,summary:"电脑 · list_apps"})});expect(label.detail).toBe("");expect(label.label).not.toContain("list_apps");}
  });
  test("tool failures only appear in expanded details, never in activity headings or previews",()=>{
    const failure="ONLY_IN_DETAILS: process could not start";
    const tool={seq:1,...result({name:"terminal__terminal_exec",error:failure,summary:failure,outcome:{command:"bun test"}})};
    const label=activityLabel(tool);
    expect(label).toEqual(activityLabel({...tool,error:null}));
    expect(label.label).not.toContain(failure);expect(label.detail).toBe("bun test");
    const closed=renderToStaticMarkup(<Activities actorId="root" tools={[tool]} onActor={()=>{}}/>);
    expect(closed).not.toContain(failure);expect(closed).not.toContain(i18n.t("conversation_content.incomplete"));
    expect(closed).not.toContain("dl-step-error-state");
    expect(activityLabel({...tool,name:"extension__inspect",outcome:{}}).detail).toBe("");
    const details=renderToStaticMarkup(<ToolDetail result={tool}/>);
    expect(details).toContain(failure);
  });
  test("long diffs retain source lines and expose a scroll boundary rather than a more-lines button", () => {
    const lines = Array.from({length:90},(_,index)=>({kind:index%2?"remove":"add",old_line:1000+index,new_line:1000+index,text:index===0?"":`SOURCE_${index}  <literal>\t${"Long text ".repeat(30)}`}));
    const html = renderToStaticMarkup(<ToolDetail result={result({output_schema:changeSchema,name:"files__edit_file",payload:{path:"notes.md",change:{added_lines:45,removed_lines:45,lines}}})}/>);
    expect(html.match(/<tr\b/g)).toHaveLength(60);
    const text = html.replace(/<[^>]*>/g, "");
    expect(text).toContain("SOURCE_59");
    expect(text).not.toContain("SOURCE_60");
    expect(html).toContain("&lt;literal&gt;");
    expect(html).toContain("data-lazy-page");
    expect(html.match(/<button\b/g)).toHaveLength(1); // Copy is the only action inside the diff.
  });
  test("changed-file results reveal subsequent files through scrolling without an extra action", () => {
    const tools = Array.from({length:7},(_,index)=>({id:`change-${index}`,call_id:`call-${index}`,message_id:"message",output_schema:changeSchema,name:"files__edit_file",summary:"",error:null,status:"completed" as const,outcome:{path:`file-${index}.ts`,added_lines:1,removed_lines:0}}));
    const html=renderToStaticMarkup(<MessageOutcomes actorId="root" tools={tools} onActor={()=>{}}/>);
    expect(html).toContain("file-2.ts");
    expect(html).not.toContain("file-3.ts");
    expect(html).toContain("data-lazy-page");
    expect(html.match(/<button\b/g)).toHaveLength(4); // View changes and the first three file links.
  });
  test("sandbox bypass preserves original icons, summaries and typed contents without parsing wrapper arguments",()=>{
    const examples=[
      {name:"terminal__terminal_exec",outcome:{command:"repair-service"},payload:{output:"original stdout",session:{command:"repair-service",exit_code:0}},schema:{properties:{output:{"x-dl-content":"text"},session:{properties:{command:{},exit_code:{}}}}}},
      {name:"files__edit_file",outcome:{path:"file.rs",added_lines:1,removed_lines:1},payload:{path:"file.rs",change:{added_lines:1,removed_lines:1,lines:[{kind:"add",old_line:null,new_line:1,text:"original source"}]}},schema:changeSchema},
      {name:"computer_use__get_window_state",outcome:{},payload:{observation:{title:"original window",url:"https://example.com"}},schema:guiSchema},
    ];
    for(const example of examples) {
      const direct={seq:1,...result({name:example.name,payload:example.payload as JsonValue,output_schema:example.schema as JsonValue,outcome:example.outcome as JsonValue})};
      const wrapped={...direct,name:"sudo__execute_as_admin",arguments:{arguments_json:"DO_NOT_PARSE_PRIVATE_ARGUMENTS"},payload:{...example.payload,sudo:{approval_id:"PRIVATE_ID"}} as JsonValue,outcome:{...example.outcome,operation:example.name} as JsonValue};
      expect(operationToolName(wrapped)).toBe(example.name);
      expect(toolIcon(wrapped.name,wrapped.outcome)).toBe(toolIcon(direct.name,direct.outcome));
      expect(activityLabel(wrapped)).toEqual(activityLabel(direct));
      expect(renderToStaticMarkup(<ToolDetail result={wrapped}/>)).toBe(renderToStaticMarkup(<ToolDetail result={direct}/>));
      expect(messageOutcomeTools([wrapped]).changes.length).toBe(example.name==="files__edit_file"?1:0);
    }
    const running={seq:1,...result({id:"live:sudo",name:"sudo__execute",outcome:{operation:"terminal__terminal_exec",command:"repair-service"}}),status:"running" as const};
    const html=renderToStaticMarkup(<Activities actorId="root" tools={[running]} onActor={()=>{}}/>);
    expect(html).toContain(`${i18n.t("tool_presentation.running")} repair-service`);
    expect(html).not.toContain(i18n.t("tool_presentation.sudo_execute"));
  });
  test("Sudo preserves target command output and keeps submitted applications separate from effects", () => {
    const html = renderToStaticMarkup(<ToolDetail result={result({ name:"sudo__execute", arguments:{tool:"terminal__terminal_exec",arguments_json:JSON.stringify({command:"winget --version"})},payload:{output:"v1.12.350",session:{command:"winget --version",exit_code:0},sudo:{tool:"terminal__terminal_exec",administrator:false,approval_id:"PRIVATE_APPROVAL"}}})}/>);
    expect(html).not.toContain("winget --version");
    expect(html).toContain("v1.12.350");
    for(const hidden of ["PRIVATE_APPROVAL","HIDDEN_MODEL_PROTOCOL","arguments_json"]) expect(html).not.toContain(hidden);
    const submitted = renderToStaticMarkup(<ToolDetail result={result({name:"sudo__execute_as_admin",output_schema:{properties:{status:{"x-dl-content":"status"},executed:{}}},payload:{status:"submitted",executed:false,approval_id:"PRIVATE_APPROVAL"}})}/>);
    expect(submitted).toContain(i18n.t("observation_status.submitted"));
    expect(submitted).not.toContain("PRIVATE_APPROVAL");
    const custom = renderToStaticMarkup(<ToolDetail result={result({name:"extension__inspect",payload:{text:"Inspection complete",sandbox:{denials:[{resource:"PRIVATE_NATIVE_TARGET"}]},sudo:{approval_id:"PRIVATE_APPROVAL"}}})}/>);
    expect(custom).toContain("Inspection complete");
    for(const hidden of ["PRIVATE_NATIVE_TARGET","PRIVATE_APPROVAL"]) expect(custom).not.toContain(hidden);
  });
  test("image tools open authorized thumbnails in a dialog instead of another tab or metadata", () => {
    const uri = "/v1/actors/root/resources/image-snapshot";
    const stage = "12345678-1234-1234-1234-123456789012";
    for (const name of ["files__view_image", "actors__read_resource", "extension__render", "mcp_00000000000000000000000000000001__lookup", "d_abcdef123456_files__view_image"]) {
      for (const source of [uri, `/v1/stages/${stage}/actors/root/resources/image-snapshot`]) {
        const html = renderToStaticMarkup(<ToolDetail result={result({name,output_schema:imageSchema,payload:{path:"C:/images/photo.png",media_type:"image/png",byte_count:9234,resource_references:[{id:"PRIVATE_RESOURCE_ID",uri:source,name:"photo.png",media_type:"image/png"}]}})} />);
        expect(html).toContain("<img ");
        expect(html).toContain(`src="${source}"`);
        expect(html).toContain('aria-haspopup="dialog"');
        expect(html).toContain('alt="photo.png"');
        for (const hidden of ["PRIVATE_RESOURCE_ID", "resource_references", "HIDDEN_MODEL_PROTOCOL"]) expect(html).not.toContain(hidden);
        if (name.endsWith("files__view_image")) {
          expect(html).not.toContain("9234");
          expect(html).not.toContain("image/png");
        }
      }
    }
  });
  test("image previews deduplicate resource references and never load external or filesystem images", () => {
    const uri = "/v1/actors/root/resources/image-snapshot";
    const reference = {uri,name:"photo.png",media_type:"image/png"};
    const html = renderToStaticMarkup(<ToolDetail result={result({name:"files__view_image",payload:{resource_references:[reference,reference,
      {uri:"https://outside.example/private.png",name:"EXTERNAL_SENTINEL",media_type:"image/png"},
      {uri:"file:///C:/private.png",name:"FILE_SENTINEL",media_type:"image/png"},
      {uri:"/v1/actors/root/resources/document",name:"TEXT_SENTINEL",media_type:"text/plain"}]}})} />);
    expect(html.match(/<img\b/g)).toHaveLength(1);
    expect(html).toContain(`src="${uri}"`);
    for (const hidden of ["outside.example", "file:///", "EXTERNAL_SENTINEL", "FILE_SENTINEL", "TEXT_SENTINEL"]) expect(html).not.toContain(hidden);
  });
  test("GUI screenshots retain previews through either screenshot URLs or resource references", () => {
    const uri = "/v1/actors/root/resources/screenshot";
    for (const name of ["computer_use__get_window_state", "phone_use__get_window_state", "browser_use__observe"]) {
      for (const references of [[], [{uri,name:"image",media_type:"image/png"}]]) {
        const html = renderToStaticMarkup(<ToolDetail result={result({name,payload:{status:"observed",observation:{title:"Observed application",screenshot_url:uri},resource_references:references}})} />);
        expect(html.match(/<img\b/g)).toHaveLength(1);
        expect(html).toContain(`src="${uri}"`);
        expect(html).toContain('aria-haspopup="dialog"');
        expect(html).toContain('alt="Observed application"');
      }
    }
  });
  test("permission application and grant render as permission states without claiming execution", () => {
    for (const decision of ["application_required", "approved"] as const) {
      const approval = {decision,approval_id:"PRIVATE_APPROVAL_ID",stage_id:"PRIVATE_STAGE_ID",mode:"auto"};
      const key = decision === "approved" ? "permission_approved" : "application_required";
      const tool = {seq:1,...result({name:"terminal__terminal_exec",payload:{approval,executed:false},outcome:{approval},summary:"PRIVATE_RUNTIME_GUIDANCE"})};
      expect(approvalNotice(tool.outcome)?.kind).toBe(decision);
      expect(activityLabel(tool).label).toBe(i18n.t(`tool_details.${key}`));
      const html = renderToStaticMarkup(<ToolDetail result={tool}/>);
      expect(html).toContain(`aria-label="${i18n.t(`tool_details.${key}`)}"`);
      expect(html).toContain(i18n.t(`tool_details.${key}_reason`));
      const activities = renderToStaticMarkup(<Activities actorId="root" tools={[tool]} onActor={()=>{}}/>);
      expect(activities).toContain(i18n.t("tool_details.permission_status"));
      for (const hidden of ["PRIVATE_APPROVAL_ID", "PRIVATE_STAGE_ID", "PRIVATE_RUNTIME_GUIDANCE", "HIDDEN_MODEL_PROTOCOL", 'role="alert"']) {
        expect(html).not.toContain(hidden);
        expect(activities).not.toContain(hidden);
      }
    }
  });
  test("approval rejection renders its reason without failures or raw approval metadata for any App", () => {
    const approval = {decision:"denied",approval_id:"PRIVATE_APPROVAL_ID",stage_id:"PRIVATE_STAGE_ID",mode:"auto",reason:"Need confirmation for <output>\nChoose an authorized folder."};
    for (const name of ["files__write_file", "terminal__terminal_exec", "computer_use__click", "actors__create", "mcp_00000000000000000000000000000001__lookup"]) {
      const tool = {seq:1,...result({name,payload:{approval},outcome:{approval}})};
      expect(activityLabel(tool).detail).toBe(approval.reason);
      const html = renderToStaticMarkup(<ToolDetail result={tool}/>);
      expect(html).toContain("Need confirmation for &lt;output&gt;");
      expect(html).toContain("Choose an authorized folder.");
      for (const hidden of ["PRIVATE_APPROVAL_ID", "PRIVATE_STAGE_ID", "HIDDEN_MODEL_PROTOCOL", 'role="alert"']) expect(html).not.toContain(hidden);
      const activities = renderToStaticMarkup(<Activities actorId="root" tools={[tool]} onActor={()=>{}}/>);
      expect(activities).not.toContain('class="dl-step-error-state"');
    }
  });
  test("ComputerUse renders source window facts without raw input or observation identifiers", () => {
    const html = renderToStaticMarkup(<ToolDetail result={result({output_schema:guiSchema,name:"computer_use__give_window",arguments:{text:"PRIVATE_TYPED_TEXT"},payload:{status:"given_to_user",windows:[{title:"Returned document",controlled:false}],private_metadata:"PRIVATE_DRIVER_METADATA",state_id:"PRIVATE_STATE_TOKEN"}})}/>);
    expect(html).toContain("Returned document");
    for(const hidden of ["PRIVATE_TYPED_TEXT","PRIVATE_DRIVER_METADATA","PRIVATE_STATE_TOKEN","HIDDEN_MODEL_PROTOCOL"]) expect(html).not.toContain(hidden);
  });
  test("BrowserUse renders page facts while keeping coordinates and native receipts private", () => {
    const record=result({output_schema:guiSchema,name:"browser_use__click",arguments:{text:"PRIVATE_TYPED_TEXT",state_id:"PRIVATE_RECEIPT"},payload:{status:"observed",observation:{title:"Shared browser document",url:"https://example.com/report",tab_id:"PRIVATE_TAB",state_id:"PRIVATE_RECEIPT",png_base64:"PRIVATE_IMAGE"},private_metadata:"PRIVATE_NATIVE"}});
    const html=renderToStaticMarkup(<ToolDetail result={record}/>);
    expect(toolResultTitle(record)).toBe("Shared browser document");
    expect(html).toBe(""); // Titles stay in the row; no redundant disclosure.
    for(const hidden of ["PRIVATE_TYPED_TEXT","PRIVATE_RECEIPT","PRIVATE_TAB","PRIVATE_IMAGE","PRIVATE_NATIVE","HIDDEN_MODEL_PROTOCOL"])expect(html).not.toContain(hidden);
  });
  test("PhoneUse renders window observations without private native identifiers or typed input", () => {
    const html = renderToStaticMarkup(<ToolDetail result={result({output_schema:guiSchema,name:"phone_use__set_text",arguments:{text:"PRIVATE_TYPED_TEXT"},payload:{status:"dispatched",observation:{title:"Android document",width:1280,height:720,state_id:"PRIVATE_STATE_TOKEN",window_id:"PRIVATE_WINDOW_TOKEN",input_target:{kind:"editable_control",text:"PRIVATE_RECEIVER_TEXT"}},private_metadata:"PRIVATE_NATIVE_METADATA"}})}/>);
    expect(html).toBe(""); // The observation title belongs in its compact row, with no empty disclosure.
    for (const hidden of ["PRIVATE_TYPED_TEXT","PRIVATE_STATE_TOKEN","PRIVATE_WINDOW_TOKEN","PRIVATE_NATIVE_METADATA","HIDDEN_MODEL_PROTOCOL"]) expect(html).not.toContain(hidden);
  });
  test("MCP errors preserve source text without exposing binary or protocol fields", () => {
    const name="mcp_00000000000000000000000000000001__t1234567890";
    const html=renderToStaticMarkup(<ToolDetail result={result({name,output_schema:mcpSchema,error:"remote_error",payload:{content:[{type:"text",text:"<script>record rejected</script>"},{type:"image",data:"BINARY_SENTINEL",mimeType:"image/png"}],app:{name:"Example",tool:"lookup"},private_metadata:"PRIVATE_SENTINEL"}})}/>);
    expect(html).toContain("&lt;script&gt;record rejected&lt;/script&gt;");
    for(const hidden of ["BINARY_SENTINEL","PRIVATE_SENTINEL","HIDDEN_MODEL_PROTOCOL","remote_error"]) expect(html).not.toContain(hidden);
    const label=activityLabel({seq:1,...result({name,outcome:{app:{name:"Example",tool:"lookup.with space"}}})});
    expect(label.label).toBe("Example");expect(label.detail).toBe("lookup.with space");
  });
  test("MCP structured results preserve original values without adapter JSON", () => {
    const html=renderToStaticMarkup(<ToolDetail result={result({name:"mcp_00000000000000000000000000000001__lookup",payload:{structuredContent:{path:"exact <file>",value:null}}})}/>);
    expect(html).toContain("exact &lt;file&gt;");expect(html).not.toContain("arguments_json");expect(html).not.toContain("&quot;path&quot;");expect(html).toContain(i18n.t("tool_parameters.not_specified"));
  });
  test("resource readers do not expose serialized tool output", () => {
    const html = renderToStaticMarkup(<ToolDetail result={result({name:"actors__read_resource",output_schema:{properties:{}},payload:{content:'{"internal":"HIDDEN_COMPLETE_OUTPUT"}',resource_id:"opaque-id"}})} />);
    expect(html).not.toContain("HIDDEN_COMPLETE_OUTPUT");
    expect(html).not.toContain("opaque-id");
  });
  test("command activity starts collapsed and keeps source facts separate from serialized summaries", () => {
    const tool = {seq:1,...result({name:"d_abcdef123456_terminal__terminal_exec",summary:'{"internal":"HIDDEN_SUMMARY"}',outcome:{command:"cargo check"}})};
    expect(activityLabel(tool).detail).toBe("cargo check");
    const html = renderToStaticMarkup(<Activities actorId="root" tools={[tool]} onActor={()=>{}}/>);
    expect(html).toContain('aria-expanded="false"');
    expect(html).not.toContain("cargo check");
    expect(html).not.toContain("HIDDEN_SUMMARY"); expect(html).not.toContain("HIDDEN_MODEL_PROTOCOL");
  });
  test("unknown tools retain a readable outcome while hiding internal envelopes", () => {
    const html = renderToStaticMarkup(<ToolDetail result={result({})} />);
    expect(html).toContain("检查已完成");
    for (const value of ["private-result-id", "HIDDEN_PAYLOAD", "HIDDEN_MODEL_PROTOCOL", "private_metadata"])
      expect(html).not.toContain(value);
  });
  test("JSON-shaped summaries and generic content do not become a fallback inspector", () => {
    const html = renderToStaticMarkup(<ToolDetail result={result({ summary: '{"receipt":"INTERNAL_RECEIPT"}', payload: { content: '{"nested":"INTERNAL_CONTENT"}' } })} />);
    expect(html).not.toContain("INTERNAL_RECEIPT");
    expect(html).not.toContain("INTERNAL_CONTENT");
    expect(readableSummary('```json\n{"receipt":1}\n```')).toBe("");
  });
  test("real JSON file content remains intact as source, without its tool envelope", () => {
    const html = renderToStaticMarkup(<ToolDetail result={result({ output_schema:sourceSchema,name:"files__read_file", payload: { path: "settings.json", content: '{"enabled":"FILE_VALUE_SENTINEL"}', sha256: "HIDDEN_HASH", start_line: 1, end_line: 1, total_lines: 1 } })} />);
    expect(html).toContain("settings.json");
    expect(html).toContain("enabled");
    expect(html).toContain("FILE_VALUE_SENTINEL");
    expect(html).not.toContain("HIDDEN_HASH");
    expect(html).not.toContain("HIDDEN_MODEL_PROTOCOL");
  });
  test("terminal details show output without request controls or duplicated session metadata", () => {
    const html = renderToStaticMarkup(<ToolDetail result={result({ name: "terminal__terminal_exec", payload: { session: { command: "cargo check", cwd: "C:/project", exit_code: 0 }, output: "Finished build" } })} />);
    expect(html).toContain("Finished build");
    for(const hidden of ["cargo check","C:/project","max_chars","yield_time_ms"])expect(html).not.toContain(hidden);
    expect(html).not.toContain("HIDDEN_MODEL_PROTOCOL");
  });
  test("Web documents render their page content without model-specific transcripts", () => {
    const html = renderToStaticMarkup(<ToolDetail result={result({ output_schema:webSchema,name: "web__run", payload: { results:[{operation:"open",result:{ref_id:"web:example",url:"https://example.com",title:"文档",content:"CAPTURED_PAGE_TEXT",offset:0,next_offset:100}}] } })} />);
    expect(html).toContain("https://example.com");
    expect(html).toContain("文档");
    expect(html).toContain("CAPTURED_PAGE_TEXT");
    expect(html).not.toContain("web:example");
    expect(html).not.toContain("HIDDEN_MODEL_PROTOCOL");
  });
  test("JSON web documents render as inspectable values rather than a raw dump", () => {
    const html = renderToStaticMarkup(<ToolDetail result={result({ output_schema:webSchema,name: "web__run", payload: { results:[{operation:"open",result:{url:"https://example.com/data",offset:0,next_offset:null,media_type:"application/json",content:'{"title":"报告","refresh":false}',structured_content:{title:"报告",refresh:false}}}] } })} />);
    expect(html).toContain("报告");
    expect(html).toContain(i18n.t("tool_parameters.no"));
    expect(html).not.toContain("&quot;refresh&quot;");
    expect(html).not.toContain("HIDDEN_MODEL_PROTOCOL");
  });
  test("structured scalar facts preserve exact values and absence without serialization", () => {
    const args = Object.freeze({ command: "echo <danger>", max_chars: 0, refresh: false, expected_sha256: null });
    const html = renderToStaticMarkup(<ToolParameters value={args} />);
    for (const value of ["echo &lt;danger&gt;"]) expect(html).toContain(value);
    expect(html).toContain(i18n.t("tool_parameters.not_specified"));
    expect(html).toContain(i18n.t("tool_parameters.no"));
    expect(html).toContain("<span>0</span>");
    expect(html).not.toContain("expected_sha256");
    expect(html).not.toContain("<danger>");
  });
  test("nested structured data has a disclosure rather than an encoded object", () => {
    const html = renderToStaticMarkup(<ToolParameters value={{ environment: { HOME: "/workspace", FLAG: "on" } }} />);
    expect(html).toContain('aria-expanded="false"');
    expect(html).not.toContain("&quot;HOME&quot;");
  });
});
