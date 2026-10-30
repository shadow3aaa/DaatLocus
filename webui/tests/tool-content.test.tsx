import { t } from "../src/lib/i18n";
import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { ToolDetail } from "../src/components/tool-details";
import { ToolActivityRow } from "../src/components/tool-activity-row";
import { isBuiltinTool, isInternalTool, toolHasContent, toolResultTitle } from "../src/lib/tool-content";
import { toolExamples, toolExampleResult } from "../mock/tool-showcase";

test("builtin terminal rendering preserves captured bytes and omits model annotations and control fields",()=>{
  const result=toolExampleResult(toolExamples.find(item=>item.id==="terminal-controls")!);
  const html=renderToStaticMarkup(<ToolDetail result={result}/>);
  expect(html).toContain("Compiling example v1.0.0");
  for(const forbidden of ["MODEL_ONLY","max_chars","yield_time_ms","poll","6000","30000","dl-parameter-fields"])expect(html).not.toContain(forbidden);
});
test("successful coordination is an ordinary row, and errors keep useful details",()=>{
  for(const id of ["actor-create","actor-list","actor-history","actor-archive","actor-stop"]){
    const result=toolExampleResult(toolExamples.find(item=>item.id===id)!);
    expect(isInternalTool(result)).toBe(true);
    const html=renderToStaticMarkup(<ToolActivityRow actorId="research" tool={{...result,seq:1}} onActor={()=>{}}/>);
    expect(html).not.toContain("<button");expect(html).not.toContain("aria-expanded");
    expect(renderToStaticMarkup(<ToolDetail result={result}/>)).toBe("");
  }
  const failure=toolExampleResult(toolExamples.find(item=>item.id==="actor-error")!);
  expect(isInternalTool(failure)).toBe(false);expect(toolHasContent(failure)).toBe(true);
});
test("empty builtin results offer no blank disclosure while screenshots and failures remain visible",()=>{
  for(const id of ["file-read-empty","file-directory-empty","file-search-empty","file-image-none","terminal-empty","browser-list-empty","web-empty","apps-empty","file-diff-empty","file-binary"]){
    const result=toolExampleResult(toolExamples.find(item=>item.id===id)!);
    expect(toolHasContent(result)).toBe(false);
    expect(renderToStaticMarkup(<ToolDetail result={result}/>)).toBe("");
  }
  for(const id of ["file-image","computer-error","browser-closed","terminal-nonzero"])expect(toolHasContent(toolExampleResult(toolExamples.find(item=>item.id===id)!))).toBe(true);
});
test("browser labels use document facts instead of native identifiers or a serialized summary",()=>{
  const result=toolExampleResult(toolExamples.find(item=>item.id==="browser-open_page")!);
  result.summary="browser_head INTERNAL";
  expect(toolResultTitle(result)).toBe("Daat Locus · 浏览器预览");
  const html=renderToStaticMarkup(<ToolDetail result={result}/>);
  for(const forbidden of ["browser_head","page_id","state_id","resource_references"])expect(html).not.toContain(forbidden);
});

test("empty web collections and whitespace do not create a disclosure",()=>{
  const result=toolExampleResult(toolExamples.find(item=>item.id==="web-empty")!);
  result.payload={results:[{result:{title:"  ",content:"\n",matches:[],structured_content:{items:[]}}}]};
  expect(toolHasContent(result)).toBe(false);
});

test("a GUI title alone stays in its row, while uncertain effects remain inspectable",()=>{
  const result=toolExampleResult(toolExamples.find(item=>item.id==="browser-open_page")!);
  result.payload={observation:{title:"文档",url:"https://example.com/"},pages:[{title:"无关的标签"}]};
  expect(toolHasContent(result)).toBe(false);
  expect(renderToStaticMarkup(<ToolDetail result={result}/>)).toBe("");
  result.payload={...result.payload,status:"outcome_unknown"};
  expect(toolHasContent(result)).toBe(true);
  const html=renderToStaticMarkup(<ToolDetail result={result}/>);
  expect(html).toContain(t("observation_status.outcome_unknown"));
  expect(html).not.toContain("无关的标签");
});

test("every builtin preview hides request envelopes and never offers a blank body",()=>{
  for(const example of toolExamples) {
    const result=toolExampleResult(example);
    if(!isBuiltinTool(result))continue;
    result.arguments={max_chars:"PRIVATE_ARGUMENT",state_id:"PRIVATE_STATE",timeout:"PRIVATE_TIMEOUT"};
    const html=renderToStaticMarkup(<ToolDetail result={result}/>);
    for(const hidden of ["PRIVATE_ARGUMENT","PRIVATE_STATE","PRIVATE_TIMEOUT","PRIVATE_MODEL_TRANSCRIPT"])expect(html).not.toContain(hidden);
    if(toolHasContent(result))expect(html.replace(/<[^>]*>/g,"").trim()||html.includes("<img")).toBeTruthy();
  }
});

test("Sudo wrapping an installed external tool preserves its generic result renderer",()=>{
  const result=toolExampleResult(toolExamples.find(item=>item.id==="mcp-text")!);
  const target=result.name;
  result.name="sudo__execute";
  result.outcome={operation:target};
  expect(isBuiltinTool(result)).toBe(false);
  expect(renderToStaticMarkup(<ToolDetail result={result}/>)).toContain("第一段工具输出");
});
