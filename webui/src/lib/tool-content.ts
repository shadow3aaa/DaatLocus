import type { JsonValue, ToolResult, ToolSummary } from "./actor-api";
import { asObject, asRows, asText, domainToolName, operationToolName, approvalNotice, isBuiltinTool } from "./tool-presentation";
import { resourceActorId, safeLink } from "./conversation-links";

export { isBuiltinTool } from "./tool-presentation";
/** Internal coordination has no human-facing body. Failed calls still expose their error. */
export function isInternalTool(tool:Pick<ToolSummary,"name"|"outcome"|"error">) {
  const name=operationToolName(tool);
  return !tool.error&&!approvalNotice(tool.outcome)&&(
    (name.startsWith("actors__")||name.startsWith("admin_actors__"))&&name!=="actors__read_resource" ||
    ["web__close","browser_use__close_page","computer_use__close_window","computer_use__release_window","phone_use__close_window","phone_use__release_window","apps_manager__refresh","apps_manager__enable","apps_manager__close","apps_manager__configure_timeouts"].includes(name)
  );
}
/** Current source types declare their domain; names only locate older untyped records. */
export function toolContentKind(result:ToolResult):string|undefined {
  const value=asObject(result.payload);
  if(operationToolName(result)==="actors__read_resource")return "image";
  const kind=asText(asObject(result.output_schema)["x-dl-content"]);
  if(kind)return kind;
  const wrapped=asText(asObject(value.sudo).tool);
  const name=wrapped&&!wrapped.startsWith("sudo__")?domainToolName(wrapped):operationToolName(result);
  if(name.startsWith("terminal__"))return "terminal";
  if(name.startsWith("web__"))return "web";
  if(/^(browser_use|computer_use|phone_use)__/.test(name))return "gui";
  if(name.startsWith("apps_manager__"))return "apps";
  if((name.startsWith("actors__")||name.startsWith("admin_actors__")))return "actor";
  if(name.startsWith("workspace__"))return "workspace";
  if(/^(files|super_skills)__/.test(name)) {
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
  const resource=asObject(payload.resource);
  const fallback=asText(observation.title)||asText(resource.name)||asText(payload.title)||asText(payload.path).split(/[\\/]/).at(-1)||"Image";
  const seen=new Set<string>();
  return [...asRows(payload.resource_references),resource,{uri:observation.screenshot_url,name:fallback,media_type:"image/"},...(["image","svg"].includes(asText(payload.kind))?[{uri:payload.uri,name:fallback,media_type:"image/"}]:[])].flatMap(reference=>{
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
export function workspaceText(value:Record<string,JsonValue>) {
  return asObject(value.entry).kind==="symlink"?asText(value.target):asText(value.text);
}
export function contentItems(kind:string|undefined,value:Record<string,JsonValue>) {
  switch(kind) {
    case "directory":return asRows(value.entries).filter(entry=>asText(entry.name).trim()||asText(entry.path).trim());
    case "search":return asRows(value.matches).filter(entry=>asText(entry.text).trim()||asText(entry.content).trim());
    case "skills":return asRows(value.skills).filter(entry=>asText(entry.name).trim()||asText(entry.description).trim());
    case "apps":return (Array.isArray(value.apps)?asRows(value.apps):[asObject(value.app)]).filter(entry=>asText(entry.name).trim()||asText(entry.description).trim());
    case "gui":return Object.keys(asObject(value.observation)).length?[]:[value.apps,value.windows,value.pages,value.discovered_windows,value.related_windows].flatMap(asRows).filter(entry=>asText(entry.name).trim()||asText(entry.title).trim()||safeLink(asText(entry.url)));
    case "web":return asRows(value.results).filter(entry=>{
      const document=asObject(entry.result);
      return asText(entry.error).trim()||asText(document.title).trim()||safeLink(asText(document.url))||asText(document.content).trim()||hasValue(document.structured_content)||asRows(document.matches).some(match=>asText(match.content).trim());
    });
    case "workspace":return asRows(value.changes??value.versions).filter(entry=>asText(entry.path).trim()||asText(entry.message).trim());
    default:return [];
  }
}
export function toolHasContent(result:ToolResult):boolean {
  if(result.error||approvalNotice(result.payload)||resultImages(result).length)return true;
  const value=asObject(result.payload),kind=toolContentKind(result);
  switch(kind) {
    case "terminal": return Boolean(terminalText(value).trim()||value.running===true||asObject(value.session).status==="running"||Number(asObject(value.session).exit_code??value.exit_code));
    case "source":case "markdown": return Boolean(asText(value.content).trim());
    case "file-change": return asRows(asObject(value.change).lines).length>0;
    case "status":return value.status==="submitted";
    case "image":case "actor":return false;
    case "gui":return guiNeedsAttention(value)||contentItems(kind,value).length>0;
    case "directory":case "search":case "skills":case "apps":case "web":return contentItems(kind,value).length>0;
    case "workspace":return Boolean(workspaceText(value).trim()||asText(value.error)||asText(value.uncertain)||contentItems(kind,value).length||Array.isArray(value.conflicts)&&value.conflicts.some(entry=>asText(entry).trim()));
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
  if(kind==="workspace")return asText(value.path)||asText(value.message);
  if(["source","file-change","image"].includes(kind??""))return asText(value.path);
  return "";
}
