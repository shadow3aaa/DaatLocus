import i18n from "../src/lib/i18n";
import { describe, expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { ToolDetail, ApprovalDetails } from "../src/components/tool-details";
import { ToolParameters } from "../src/components/tool-parameters";
import { readableSummary, activityLabel, approvalNotice } from "../src/lib/tool-presentation";
import { Activities } from "../src/components/conversation-content";
import type { ToolResult } from "../src/lib/actor-api";

const result = (patch: Partial<ToolResult>): ToolResult => ({ id: "private-result-id", actor_id: "root", message_id: "message", call_id: "call", name: "extension__inspect", arguments: {}, payload: { private_metadata: "HIDDEN_PAYLOAD" }, model_content: '{"internal":"HIDDEN_MODEL_PROTOCOL"}', summary: "检查已完成", error: null, ...patch });

describe("tool observations are product content, never protocol dumps", () => {
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
    const html = renderToStaticMarkup(<ToolDetail result={result({name:"computer_use__give_window",arguments:{text:"PRIVATE_TYPED_TEXT"},payload:{status:"given_to_user",windows:[{title:"Returned document",controlled:false}],private_metadata:"PRIVATE_DRIVER_METADATA",state_id:"PRIVATE_STATE_TOKEN"}})}/>);
    expect(html).toContain("Returned document");
    for(const hidden of ["PRIVATE_TYPED_TEXT","PRIVATE_DRIVER_METADATA","PRIVATE_STATE_TOKEN","HIDDEN_MODEL_PROTOCOL"]) expect(html).not.toContain(hidden);
  });
  test("PhoneUse renders window observations without private native identifiers or typed input", () => {
    const html = renderToStaticMarkup(<ToolDetail result={result({name:"phone_use__set_text",arguments:{text:"PRIVATE_TYPED_TEXT"},payload:{status:"dispatched",observation:{title:"Android document",width:1280,height:720,state_id:"PRIVATE_STATE_TOKEN",window_id:"PRIVATE_WINDOW_TOKEN",input_target:{kind:"editable_control",text:"PRIVATE_RECEIVER_TEXT"}},private_metadata:"PRIVATE_NATIVE_METADATA"}})}/>);
    expect(html).toContain("Android document");
    for (const hidden of ["PRIVATE_TYPED_TEXT","PRIVATE_STATE_TOKEN","PRIVATE_WINDOW_TOKEN","PRIVATE_NATIVE_METADATA","HIDDEN_MODEL_PROTOCOL"]) expect(html).not.toContain(hidden);
  });
  test("MCP errors preserve source text without exposing binary or protocol fields", () => {
    const name="mcp_00000000000000000000000000000001__t1234567890";
    const html=renderToStaticMarkup(<ToolDetail result={result({name,error:"remote_error",payload:{content:[{type:"text",text:"<script>record rejected</script>"},{type:"image",data:"BINARY_SENTINEL",mimeType:"image/png"}],app:{name:"Example",tool:"lookup"},private_metadata:"PRIVATE_SENTINEL"}})}/>);
    expect(html).toContain("&lt;script&gt;record rejected&lt;/script&gt;");
    for(const hidden of ["BINARY_SENTINEL","PRIVATE_SENTINEL","HIDDEN_MODEL_PROTOCOL","remote_error"]) expect(html).not.toContain(hidden);
    const label=activityLabel({seq:1,...result({name,outcome:{app:{name:"Example",tool:"lookup.with space"}}})});
    expect(label.label).toBe("Example");expect(label.detail).toBe("lookup.with space");
  });
  test("MCP approval inspects decoded original parameters rather than the JSON adapter", () => {
    const html=renderToStaticMarkup(<ApprovalDetails result={{arguments_json:'{"path":"exact <file>","value":null}'}} requested={{readable_roots:[],writable_roots:[],operation_details:{app:{id:"mcp_00000000000000000000000000000001",name:"Records"},tool:"lookup",parameters:{path:"exact <file>",value:null}}}}/>);
    expect(html).toContain("Records");expect(html).toContain("lookup");expect(html).toContain("exact &lt;file&gt;");
    expect(html).not.toContain("arguments_json");expect(html).not.toContain("&quot;path&quot;");
    expect(html).toContain(i18n.t("tool_parameters.not_specified"));
  });
  test("resource readers do not expose serialized tool output", () => {
    const html = renderToStaticMarkup(<ToolDetail result={result({name:"actors__read_resource",payload:{content:'{"internal":"HIDDEN_COMPLETE_OUTPUT"}',resource_id:"opaque-id"}})} />);
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
    const html = renderToStaticMarkup(<ToolDetail result={result({ name: "files__read_file", payload: { path: "settings.json", content: '{"enabled":"FILE_VALUE_SENTINEL"}', sha256: "HIDDEN_HASH", start_line: 1, end_line: 1, total_lines: 1 } })} />);
    expect(html).toContain("settings.json");
    expect(html).toContain("enabled");
    expect(html).toContain("FILE_VALUE_SENTINEL");
    expect(html).not.toContain("HIDDEN_HASH");
    expect(html).not.toContain("HIDDEN_MODEL_PROTOCOL");
  });
  test("terminal output and nested session facts render directly", () => {
    const html = renderToStaticMarkup(<ToolDetail result={result({ name: "terminal__terminal_exec", payload: { session: { command: "cargo check", cwd: "C:/project", exit_code: 0 }, output: "Finished build" } })} />);
    for (const value of ["cargo check", "C:/project", "Finished build"]) expect(html).toContain(value);
    expect(html).not.toContain("HIDDEN_MODEL_PROTOCOL");
  });
  test("browser navigation does not leak its model-specific JSON transcript", () => {
    const html = renderToStaticMarkup(<ToolDetail result={result({ name: "browser__browser_open_page", payload: { page: { url: "https://example.com", title: "文档" } } })} />);
    expect(html).toContain("https://example.com");
    expect(html).toContain("文档");
    expect(html).not.toContain("HIDDEN_MODEL_PROTOCOL");
  });
  test("JSON web documents render as inspectable values rather than a raw dump", () => {
    const html = renderToStaticMarkup(<ToolDetail result={result({ name: "browser__read_url", payload: { url: "https://example.com/data", status: 200, media_type: "application/json", body: '{"title":"报告","refresh":false}' } })} />);
    expect(html).toContain("报告");
    expect(html).toContain(i18n.t("tool_parameters.no"));
    expect(html).not.toContain("&quot;refresh&quot;");
    expect(html).not.toContain("HIDDEN_MODEL_PROTOCOL");
  });
  test("approval fields preserve exact values, absence and requested roots without serialization", () => {
    const args = Object.freeze({ command: "echo <danger>", max_chars: 0, refresh: false, expected_sha256: null });
    const html = renderToStaticMarkup(<ApprovalDetails result={args} requested={{ readable_roots: ["C:/read"], writable_roots: ["C:/write"] }} />);
    for (const value of ["echo &lt;danger&gt;", "C:/read", "C:/write"]) expect(html).toContain(value);
    expect(html).toContain(i18n.t("tool_parameters.not_specified"));
    expect(html).toContain(i18n.t("tool_parameters.no"));
    expect(html).toContain("<span>0</span>");
    expect(html).not.toContain("expected_sha256");
    expect(html).not.toContain("<danger>");
  });
  test("nested approval data has a disclosure rather than an encoded object", () => {
    const html = renderToStaticMarkup(<ToolParameters value={{ environment: { HOME: "/workspace", FLAG: "on" } }} />);
    expect(html).toContain('aria-expanded="false"');
    expect(html).not.toContain("&quot;HOME&quot;");
  });
});
