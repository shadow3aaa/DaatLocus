import { resourceSource } from "@/lib/transport";
import { t } from "@/lib/i18n";
import { useState } from "react";
import { ExternalLinkIcon } from "lucide-react";
import { Button } from "@/components/ui/button";
import { CodeBlock, Markdown } from "@/components/markdown";
import type { ApprovalScope, JsonValue, ToolResult } from "@/lib/actor-api";
import { resourceActorId, safeLink } from "@/lib/conversation-links";
import { asObject, asRows, asText, readableSummary, domainToolName } from "@/lib/tool-presentation";
import { FileChangeView } from "@/components/file-change";
import { ToolParameters } from "@/components/tool-parameters";

function TextOutput({ label, content, command, context, href }: { label: string; content: string; command?: string; context?: string; href?: string | null }) {
  return <section className="dl-tool-output"><header><span>{label}</span>{context && <span className="dl-output-context" title={context}>{context}</span>}{href && <a href={href} target="_blank" rel="noreferrer" aria-label={t("tool_details.open_value0", {value0:context || href})}><ExternalLinkIcon aria-hidden="true" /></a>}</header><div className="dl-output-scroll" tabIndex={0} role="region" aria-label={label}>{command && <div className="dl-output-command"><span aria-hidden="true">$</span><code>{command}</code></div>}<pre>{content}</pre></div></section>;
}

function domainFields(payload: Record<string,JsonValue>): JsonValue {
  return Object.fromEntries(Object.entries(payload).filter(([key]) => !["private_metadata","model_content","model_transcript","debug","raw_response"].includes(key)).map(([key,value]) => {
    if (typeof value === "string" && /^[\[{]/.test(value.trim())) { try { const decoded: unknown = JSON.parse(value); if (decoded && typeof decoded === "object") return [key, decoded as JsonValue]; } catch { /* Literal content stays literal. */ } }
    return [key,value];
  }));
}

function ArtifactView({ result }: { result: ToolResult }) {
  const payload = asObject(result.payload);
  const uri = asText(payload.uri);
  const title = asText(payload.title) || t("tool_details.deliver_result");
  const kind = asText(payload.kind);
  const [expanded, setExpanded] = useState(false);
  const local = Boolean(resourceActorId(uri, location.href));
  const safe = local ? uri : safeLink(uri);
  if (!safe) return <p className="dl-tool-note">{t("tool_details.could_not_open_the_result")}</p>;
  return <section className="dl-artifact"><div className="dl-artifact-header"><span>{title}</span><a href={safe} target="_blank" rel="noreferrer" aria-label={t("tool_details.open_value0", {value0: title})}><ExternalLinkIcon /></a></div>
    {local && (kind === "image" || kind === "svg") && <img src={resourceSource(uri)} alt={title} loading="lazy" />}
    {local && kind === "html" && <><iframe src={resourceSource(uri)} sandbox="allow-scripts" referrerPolicy="no-referrer" title={title} className={expanded ? "dl-artifact-frame-expanded" : undefined} /><Button variant="ghost" size="sm" onClick={() => setExpanded(!expanded)}>{expanded ? t("tool_details.collapse_preview") : t("tool_details.expand_preview")}</Button></>}
    {readableSummary(asText(payload.description)) && <p>{readableSummary(asText(payload.description))}</p>}
  </section>;
}

export function ToolDetail({ result, onActor }: { result: ToolResult; onActor?(id: string): void }) {
  result = { ...result, name: domainToolName(result.name) };
  const payload = asObject(result.payload);
  const args = asObject(result.arguments);
  const path = asText(payload.path) || asText(args.path);
  if (/^mcp_[a-f0-9]{32}__/.test(result.name)) {
    const resources = asRows(payload.resources);
    return <div className="dl-tool-details dl-mcp-details">
      {result.error && <p className="dl-operation-error">{t("tool_details.action_failed")}</p>}
      {asRows(payload.content).filter(part=>part.type === "text").map((part,index)=><pre key={index}>{asText(part.text)}</pre>)}
      {asRows(payload.content).filter(part=>part.type === "resource_link").map((part,index)=>{const uri=safeLink(asText(part.uri));return uri?<a href={uri} target="_blank" rel="noreferrer" key={index}>{asText(part.name)||uri}<ExternalLinkIcon aria-hidden="true"/></a>:null;})}
      {payload.structuredContent != null && <ToolParameters value={payload.structuredContent}/>}
      {resources.map(resource=>{const uri=asText(resource.uri);return resourceActorId(uri,location.href)?<a key={uri} href={uri} target="_blank" rel="noreferrer">{asText(resource.name)}<ExternalLinkIcon aria-hidden="true"/></a>:null;})}
    </div>;
  }
  if (result.name.startsWith("apps_manager__") && !result.error) {
    const apps=Array.isArray(payload.apps)?asRows(payload.apps):[asObject(payload.app ?? payload)];
    if (result.name === "apps_manager__refresh") return <p className="dl-tool-note">{t("apps.refresh_action")}</p>;
    return <ul className="dl-tool-result-list">{apps.filter(app=>asText(app.name)).map((app,index)=><li key={index}><strong>{asText(app.name)}</strong>{asText(app.description)&&<p>{asText(app.description)}</p>}<small>{app.enabled?t("apps.enabled"):t("apps.disabled")}</small></li>)}</ul>;
  }
  if (result.name.startsWith("computer_use__")) {
    const observation = asObject(payload.observation);
    const image = asText(observation.screenshot_url);
    const rows = [...asRows(payload.apps), ...asRows(payload.windows), ...asRows(payload.discovered_windows), ...asRows(payload.related_windows)];
    const status = asText(payload.status);
    const statusKeys: Record<string, string> = { observed: "computer_use.observed", dispatched: "computer_use.dispatched", outcome_unknown: "computer_use.outcome_unknown", handoff_pending: "computer_use.handoff_pending", given_to_user: "computer_use.given_to_user" };
    return <section className="dl-tool-details">
      {result.error && <p className="dl-operation-error">{readableSummary(result.error)}</p>}
      {asText(observation.title) && <p>{asText(observation.title)}</p>}
      {statusKeys[status] && <p className="dl-tool-note">{t(statusKeys[status])}</p>}
      {image && resourceActorId(image, location.href) && <div className="dl-artifact"><img src={resourceSource(image)} alt={asText(observation.title) || t("computer_use.window_image")} loading="lazy" /></div>}
      {rows.length > 0 && <ul className="dl-tool-result-list">{rows.map((row, index) => <li key={asText(row.window_id) || asText(row.app_id) || index}><strong>{asText(row.title) || asText(row.name)}</strong>{typeof row.taken === "boolean" && <small>{t(row.taken ? "computer_use.controlled" : "computer_use.not_controlled")}</small>}</li>)}</ul>}
    </section>;
  }
  if (result.error) return <p className="dl-operation-error">{readableSummary(result.error) || t("tool_details.action_failed")}</p>;
  if (result.name.startsWith("terminal__")) {
    const session = asObject(payload.session);
    const command = asText(payload.command) || asText(session.command) || asText(args.command);
    const output = asText(payload.stdout) || asText(payload.output);
    const stderr = asText(payload.stderr);
    const cwd = asText(payload.cwd) || asText(session.cwd);
    const exit = payload.exit_code ?? session.exit_code;
    return <div className="dl-terminal-details"><TextOutput label="Shell" command={command} context={cwd} content={`${output}${stderr ? `\n${stderr}` : ""}`} />{typeof exit === "number" && exit !== 0 && <small>{t("tool_details.exit_code_value", {value:exit})}</small>}{payload.running === true && <small>{t("tool_details.command_is_running")}</small>}</div>;
  }
  if (result.name === "artifacts__present") return <ArtifactView result={result} />;
  if (result.name === "actors__read_resource") return <p className="dl-tool-note">{t("tool_details.read_attachment_or_complete_output")}</p>;
  if (["files__read_file", "files__read_skill"].includes(result.name)) {
    const source = asText(payload.content) || asText(payload.text);
    return <div className="dl-file-details"><CodeBlock code={source} language={path || "text"} startLine={typeof payload.start_line === "number" ? payload.start_line : undefined} />{typeof payload.start_line === "number" && typeof payload.end_line === "number" && <small>{t("tool_details.lines")}{payload.start_line}–{payload.end_line} {t("file_change.lines")}{typeof payload.total_lines === "number" && t("tool_details.value0_lines_total", {value0: payload.total_lines})}</small>}</div>;
  }
  if (["files__write_file", "files__edit_file", "files__copy_file"].includes(result.name)) return <FileChangeView path={path || asText(args.destination)} value={payload.change} />;
  if (result.name === "files__list_directory") return <div className="dl-file-details"><code>{path}</code><ul>{asRows(payload.entries).map((entry, index) => <li key={index}>{asText(entry.name)}{entry.kind === "directory" && "/"}</li>)}</ul>{payload.next_cursor != null && <small>{t("tool_details.file_contents_displayed")}</small>}</div>;
  if (result.name === "files__search_text") return <div className="dl-file-details"><p className="dl-tool-note">{t("tool_details.search")}{asText(args.query)}</p><ul className="dl-file-matches">{asRows(payload.matches).map((match, index) => <li key={index}><small>{asText(match.path)}:{String(match.line_number)}</small><pre>{asText(match.text)}</pre></li>)}</ul>{payload.limited === true && <small>{t("tool_details.result_limit_reached")}</small>}</div>;
  if (result.name === "files__list_skills") return <ul className="dl-tool-result-list">{asRows(payload.skills).map((skill, index) => <li key={index}><strong>{asText(skill.name)}</strong><p>{readableSummary(asText(skill.description))}</p></li>)}</ul>;
  if (result.name === "actors__list") return <ul className="dl-tool-result-list">{asRows(payload.actors).map((actor, index) => <li key={index}>{onActor && asText(actor.id) ? <button className="dl-result-actor" type="button" onClick={() => onActor(asText(actor.id))}>{asText(actor.name) || "Actor"}</button> : <strong>{asText(actor.name) || "Actor"}</strong>}<small>{actor.read_only === true ? t("tool_details.read_only") : actor.status === "running" ? t("conversation_pane.working") : t("conversation_pane.ready")}</small></li>)}</ul>;
  if (result.name.startsWith("browser__")) {
    const page = asObject(payload.page);
    const url = asText(payload.url) || asText(page.url) || asText(args.url);
    const href = safeLink(url);
    const source = asText(payload.snapshot) || asText(payload.body);
    let document: JsonValue | undefined;
    if (asText(payload.media_type).includes("json")) { try { document = JSON.parse(source) as JsonValue; } catch { /* Invalid source remains literal text. */ } }
    return document !== undefined ? <div className="dl-tool-details dl-browser-details"><ToolParameters value={document} /></div> : <TextOutput label={t("tool_details.plain_text")} context={asText(payload.title) || asText(page.title)} content={source || readableSummary(result.summary) || t("tool_details.page_action_completed")} href={href} />;
  }
  const text = [payload.message, payload.description, payload.text, payload.content].map(value => readableSummary(asText(value))).find(Boolean);
  const fields = text ? Object.fromEntries(Object.entries(payload).filter(([key]) => !["message","description","text","content"].includes(key))) : payload;
  return <div className="dl-tool-note">{text ? <Markdown content={text} /> : <p>{readableSummary(result.summary) || t("tool_details.action_completed")}</p>}<ToolParameters value={domainFields(fields)} /></div>;
}

export function ApprovalDetails({ result, requested, review, scopes }: { result: ToolResult["arguments"]; review?: {criteria:string} | null; scopes?: ApprovalScope[]; requested: { readable_roots: string[]; writable_roots: string[]; operation_details?: ToolResult["arguments"] } }) {
  const operation=asObject(requested.operation_details ?? undefined);
  const app=asObject(operation.app);
  const source=asObject(app.source);
  const connection=asText(source.url)||asText(source.command);
  const mcp=/^mcp_[a-f0-9]{32}$/.test(asText(app.id)) && operation.parameters != null;
  const paths = scopes?.filter((scope): scope is Extract<ApprovalScope,{kind:"filesystem"}> => scope.kind === "filesystem");
  const reads = paths?.length ? paths.filter(scope=>!scope.write).map(scope=>scope.path) : requested.readable_roots;
  const writes = paths?.length ? paths.filter(scope=>scope.write).map(scope=>scope.path) : requested.writable_roots;
  const recursive = new Set(paths?.filter(scope=>scope.recursive).map(scope=>scope.path));
  return <div className="dl-approval-details">{review && <section className="dl-permission-roots"><h4>{t("tool_details.review_criteria")}</h4><p className="dl-parameter-text">{review.criteria}</p></section>}{mcp && <><p>{asText(app.name)} · {asText(operation.tool)}</p>{connection&&<p className="dl-parameter-text">{t("apps.connection")}: {connection}</p>}</>}<ToolParameters value={mcp?operation.parameters:result} />{reads.length > 0 && <section className="dl-permission-roots"><h4>{t("tool_details.additional_read_access")}</h4><ul>{reads.map(path => <li key={path}><code>{path}</code>{recursive.has(path) && <small> · {t("tool_details.includes_subdirectories")}</small>}</li>)}</ul></section>}{writes.length > 0 && <section className="dl-permission-roots"><h4>{t("tool_details.additional_write_access")}</h4><ul>{writes.map(path => <li key={path}><code>{path}</code>{recursive.has(path) && <small> · {t("tool_details.includes_subdirectories")}</small>}</li>)}</ul></section>}{requested.operation_details != null && !mcp && <section className="dl-permission-roots"><h4>{t("tool_details.execution_details")}</h4><ToolParameters value={requested.operation_details} /></section>}</div>;
}
