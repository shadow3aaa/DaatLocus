import type { JsonValue, ToolResult, ToolSummary } from "./actor-api";
import { asObject, asRows, asText, domainToolName, operationToolName, approvalNotice } from "./tool-presentation";
import { resourceActorId, safeLink } from "./conversation-links";

export function isBuiltinTool(tool:Pick<ToolSummary,"name"|"outcome">) {
  return /^(actors|admin_actors|files|terminal|web|browser_use|computer_use|phone_use|apps_manager|artifacts|sudo)__/.test(operationToolName(tool));
}
/** Internal coordination has no human-facing body. Failed calls still expose their error. */
export function isInternalTool(tool:Pick<ToolSummary,"name"|"outcome"|"error">) {
  const name=operationToolName(tool);
  return !tool.error&&!approvalNotice(tool.outcome)&&(
    (name.startsWith("actors__")||name.startsWith("admin_actors__"))&&name!=="actors__read_resource" ||
    ["web__close","browser_use__close_page","computer_use__close_window","computer_use__release_window","phone_use__close_window","phone_use__release_window","apps_manager__refresh","apps_manager__enable","apps_manager__close"].includes(name)
  );
}
/** Current source types declare their domain; names only locate older untyped records. */
export function toolContentKind(result:ToolResult):string|undefined {
  const kind=asText(asObject(result.output_schema)["x-dl-content"]);
  if(kind)return kind;
  const value=asObject(result.payload);
  const wrapped=asText(asObject(value.sudo).tool);
  const name=wrapped&&!wrapped.startsWith("sudo__")?domainToolName(wrapped):operationToolName(result);
  if(name.startsWith("terminal__"))return "terminal";
  if(name.startsWith("web__"))return "web";
  if(/^(browser_use|computer_use|phone_use)__/.test(name))return "gui";
  if(name.startsWith("apps_manager__"))return "apps";
  if((name.startsWith("actors__")||name.startsWith("admin_actors__")))return "actor";
  if(name.startsWith("files__")) {
    if(name.endsWith("view_image"))return "image";
    if(name.endsWith("list_directory"))return "directory";
    if(name.endsWith("search_text"))return "search";
    if(name.endsWith("list_skills"))return "skills";
    if(typeof value.content==="string")return "source";
    return "file-change";
  }
  if(name.startsWith("artifacts__"))return "artifact";
  if(name.startsWith("sudo__"))return "status";
}
export function resultImages(result:ToolResult) {
  const payload=asObject(result.payload),observation=asObject(payload.observation);
  const origin=typeof location==="undefined"?"http://localhost":location.href;
  const fallback=asText(observation.title)||asText(payload.path).split(/[\\/]/).at(-1)||"Image";
  const seen=new Set<string>();
  return [...asRows(payload.resource_references),{uri:observation.screenshot_url,name:fallback,media_type:"image/"}].flatMap(reference=>{
    const uri=asText(reference.uri);
    if(!asText(reference.media_type).startsWith("image/")||!resourceActorId(uri,origin)||seen.has(uri))return [];
    seen.add(uri);
    return [{uri,name:asText(reference.name)&&reference.name!=="image"?asText(reference.name):fallback}];
  });
}
export function terminalText(value:Record<string,JsonValue>) {
  if(typeof value.captured_output==="string")return value.captured_output;
  return typeof value.output==="string"?value.output:[asText(value.stdout),asText(value.stderr)].filter(Boolean).join("\n");
}
function hasValue(value:JsonValue|undefined):boolean {
  if(typeof value==="string")return Boolean(value.trim());
  if(Array.isArray(value))return value.some(hasValue);
  if(value&&typeof value==="object")return Object.values(value).some(hasValue);
  return value!==undefined&&value!==null;
}
export function guiNeedsAttention(value:Record<string,JsonValue>) {
  return value.status==="outcome_unknown"||value.status==="handoff_pending";
}
export function toolHasContent(result:ToolResult):boolean {
  if(result.error||approvalNotice(result.payload)||resultImages(result).length)return true;
  const value=asObject(result.payload),kind=toolContentKind(result);
  switch(kind) {
    case "terminal": return Boolean(terminalText(value)||value.running===true||asObject(value.session).status==="running"||Number(asObject(value.session).exit_code??value.exit_code));
    case "source": return Boolean(asText(value.content));
    case "file-change": return asRows(asObject(value.change).lines).length>0;
    case "status":return typeof value.status==="string"&&value.status!=="completed";
    case "image":case "actor":return false;
    case "directory":return asRows(value.entries).length>0;
    case "search":return asRows(value.matches).length>0;
    case "skills":return asRows(value.skills).length>0;
    case "apps":return asRows(value.apps).length>0||Boolean(asObject(value.app).description);
    case "gui":return guiNeedsAttention(value)||(!Object.keys(asObject(value.observation)).length&&[value.apps,value.windows,value.pages,value.discovered_windows,value.related_windows].some(list=>asRows(list).some(item=>item.name||item.title||item.url)));
    case "web":return asRows(value.results).some(item=>hasValue(item.error)||Object.entries(asObject(item.result)).some(([key,entry])=>["title","url","content","structured_content","matches"].includes(key)&&hasValue(entry)));
    case "artifact":return Boolean(safeLink(asText(value.uri))||resourceActorId(asText(value.uri),typeof location==="undefined"?"http://localhost":location.href));
    default:return !isBuiltinTool(result);
  }
}

/** Only titles, file paths and actual commands belong beside operation labels. */
export function toolResultTitle(result:ToolResult) {
  const value=asObject(result.payload),kind=toolContentKind(result);
  if(kind==="terminal")return asText(asObject(value.session).command)||asText(value.command);
  if(kind==="gui")return asText(asObject(value.observation).title)||asText(asObject(value.observation).url);
  if(kind==="web") { const document=asObject(asRows(value.results)[0]?.result);return asText(document.title)||asText(document.url); }
  if(kind==="apps")return asText(asObject(value.app).name)||asText(value.name);
  if(["source","file-change","image"].includes(kind??""))return asText(value.path);
  return "";
}
