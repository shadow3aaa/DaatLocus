import i18n from "../src/lib/i18n";
import { beforeEach, describe, expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { ToolDetail, ApprovalDetails } from "../src/components/tool-details";
import { ToolParameters } from "../src/components/tool-parameters";
import { readableSummary, activityLabel } from "../src/lib/tool-presentation";
import { Activities } from "../src/components/conversation-content";
import type { ToolResult } from "../src/lib/actor-api";

const result = (patch: Partial<ToolResult>): ToolResult => ({ id: "private-result-id", actor_id: "root", message_id: "message", call_id: "call", name: "extension__inspect", arguments: {}, payload: { private_metadata: "HIDDEN_PAYLOAD" }, model_content: '{"internal":"HIDDEN_MODEL_PROTOCOL"}', summary: "检查已完成", error: null, ...patch });

describe("tool observations are product content, never protocol dumps", () => {
  test("resource readers do not expose serialized tool output", () => {
    const html = renderToStaticMarkup(<ToolDetail result={result({name:"actors__read_resource",payload:{content:'{"internal":"HIDDEN_COMPLETE_OUTPUT"}',resource_id:"opaque-id"}})} />);
    expect(html).toContain("已读取附件或完整输出");
    expect(html).not.toContain("HIDDEN_COMPLETE_OUTPUT");
    expect(html).not.toContain("opaque-id");
  });
  test("compact command rows use source facts and hide serialized summaries", () => {
    const tool = {seq:1,...result({name:"d_abcdef123456_terminal__terminal_exec",summary:'{"internal":"HIDDEN_SUMMARY"}',outcome:{command:"cargo check"}})};
    expect(activityLabel(tool)).toEqual({label:"已运行",detail:"cargo check"});
    const html = renderToStaticMarkup(<Activities actorId="root" tools={[tool]} onActor={()=>{}}/>);
    expect(html).toContain("已运行"); expect(html).toContain("cargo check");
    expect(html).not.toContain("HIDDEN_SUMMARY"); expect(html).not.toContain("HIDDEN_MODEL_PROTOCOL");
    expect(activityLabel({...tool,status:"running"})).toEqual({label:"正在运行",detail:"cargo check"});
  });
  test("unknown tools retain a readable outcome while hiding internal envelopes", () => {
    const html = renderToStaticMarkup(<ToolDetail result={result({})} />);
    expect(html).toContain("检查已完成");
    for (const value of ["private-result-id", "HIDDEN_PAYLOAD", "HIDDEN_MODEL_PROTOCOL", "private_metadata"])
      expect(html).not.toContain(value);
  });
  test("JSON-shaped summaries and generic content do not become a fallback inspector", () => {
    const html = renderToStaticMarkup(<ToolDetail result={result({ summary: '{"receipt":"INTERNAL_RECEIPT"}', payload: { content: '{"nested":"INTERNAL_CONTENT"}' } })} />);
    expect(html).toContain("操作已完成");
    expect(html).not.toContain("INTERNAL_RECEIPT");
    expect(html).not.toContain("INTERNAL_CONTENT");
    expect(readableSummary('```json\n{"receipt":1}\n```')).toBe("");
  });
  test("real JSON file content remains intact as source, without its tool envelope", () => {
    const html = renderToStaticMarkup(<ToolDetail result={result({ name: "files__read_file", payload: { path: "settings.json", content: '{"enabled":true}', sha256: "HIDDEN_HASH", start_line: 1, end_line: 1, total_lines: 1 } })} />);
    expect(html).toContain("settings.json");
    expect(html).toContain("enabled");
    expect(html).not.toContain("HIDDEN_HASH");
    expect(html).not.toContain("HIDDEN_MODEL_PROTOCOL");
  });
  test("terminal output and nested session facts render directly", () => {
    const html = renderToStaticMarkup(<ToolDetail result={result({ name: "terminal__terminal_exec", payload: { session: { command: "cargo check", cwd: "C:/project", exit_code: 0 }, output: "Finished build" } })} />);
    for (const value of ["cargo check", "C:/project", "Finished build", "退出代码 0"]) expect(html).toContain(value);
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
    expect(html).toContain("否");
    expect(html).not.toContain("&quot;refresh&quot;");
    expect(html).not.toContain("HIDDEN_MODEL_PROTOCOL");
  });
  test("approval fields preserve exact values, absence and requested roots without serialization", () => {
    const args = Object.freeze({ command: "echo <danger>", max_chars: 0, refresh: false, expected_sha256: null });
    const html = renderToStaticMarkup(<ApprovalDetails result={args} requested={{ readable_roots: ["C:/read"], writable_roots: ["C:/write"] }} />);
    for (const value of ["命令", "echo &lt;danger&gt;", "未指定", "否", "C:/read", "C:/write"]) expect(html).toContain(value);
    expect(html).toContain("<span>0</span>");
    expect(html).not.toContain("expected_sha256");
    expect(html).not.toContain("<danger>");
    expect(args.command).toBe("echo <danger>");
  });
  test("nested approval data has a disclosure rather than an encoded object", () => {
    const html = renderToStaticMarkup(<ToolParameters value={{ environment: { HOME: "/workspace", FLAG: "on" } }} />);
    expect(html).toContain("环境变量");
    expect(html).toContain('aria-expanded="false"');
    expect(html).not.toContain("&quot;HOME&quot;");
  });
});

beforeEach(async () => { await i18n.changeLanguage("zh-CN"); });
