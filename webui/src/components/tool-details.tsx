import { t } from "@/lib/i18n";
import { useState } from "react";
import { ExternalLinkIcon, TerminalIcon } from "lucide-react";
import { Button } from "@/components/ui/button";
import { CodeBlock, Markdown } from "@/components/markdown";
import type { JsonValue, ToolResult } from "@/lib/actor-api";
import { resourceActorId, safeLink } from "@/lib/conversation-links";
import { asObject, asRows, asText, readableSummary, domainToolName } from "@/lib/tool-presentation";
import { FileChangeView } from "@/components/file-change";
import { elapsedLabel } from "@/lib/work-timeline";
import { ToolParameters } from "@/components/tool-parameters";

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
  const local = Boolean(resourceActorId(uri, location.origin));
  const safe = local ? uri : safeLink(uri);
  if (!safe) return <p className="dl-tool-note">{t("tool_details.could_not_open_the_result")}</p>;
  return <section className="dl-artifact"><div className="dl-artifact-header"><span>{title}</span><a href={safe} target="_blank" rel="noreferrer" aria-label={t("tool_details.open_value0", {value0: title})}><ExternalLinkIcon /></a></div>
    {local && (kind === "image" || kind === "svg") && <img src={uri} alt={title} loading="lazy" />}
    {local && kind === "html" && <><iframe src={uri} sandbox="allow-scripts" referrerPolicy="no-referrer" title={title} className={expanded ? "dl-artifact-frame-expanded" : undefined} /><Button variant="ghost" size="sm" onClick={() => setExpanded(!expanded)}>{expanded ? t("tool_details.collapse_preview") : t("tool_details.expand_preview")}</Button></>}
    {readableSummary(asText(payload.description)) && <p>{readableSummary(asText(payload.description))}</p>}
  </section>;
}

export function ToolDetail({ result, onActor }: { result: ToolResult; onActor?(id: string): void }) {
  result = { ...result, name: domainToolName(result.name) };
  const payload = asObject(result.payload);
  const args = asObject(result.arguments);
  const path = asText(payload.path) || asText(args.path);
  if (result.error) return <p className="dl-operation-error">{readableSummary(result.error) || t("tool_details.action_failed")}</p>;
  if (result.name.startsWith("terminal__")) {
    const session = asObject(payload.session);
    const command = asText(payload.command) || asText(session.command) || asText(args.command);
    const output = asText(payload.stdout) || asText(payload.output);
    const stderr = asText(payload.stderr);
    const cwd = asText(payload.cwd) || asText(session.cwd);
    const exit = payload.exit_code ?? session.exit_code;
    const elapsed = elapsedLabel(typeof session.started_at_ms === "number" ? session.started_at_ms : result.started_at_ms, typeof session.finished_at_ms === "number" ? session.finished_at_ms : result.finished_at_ms);
    return <div className="dl-tool-details dl-terminal-details">{command && <div className="dl-terminal-command"><TerminalIcon aria-hidden="true" /><code>{command}</code></div>}{cwd && <small>{cwd}</small>}{(output || stderr) && <pre aria-label={t("tool_details.command_output")}>{output}{stderr && `\n${stderr}`}</pre>}{elapsed && <small>{t("tool_details.elapsed_value", {value:elapsed})} · </small>}{exit != null ? <small>{t("tool_details.exit_code_value", {value:String(exit)})}</small> : payload.running === true ? <small>{t("tool_details.command_is_running")}</small> : <small>{t("tool_details.command_finished")}</small>}</div>;
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
    return <div className="dl-tool-details dl-browser-details">{href && <a href={href} target="_blank" rel="noreferrer">{asText(payload.title) || asText(page.title) || url}<ExternalLinkIcon aria-hidden="true" /></a>}{typeof payload.status === "number" && <small>HTTP {payload.status}</small>}{document !== undefined ? <ToolParameters value={document} /> : source ? <pre aria-label={t("tool_details.page_content")}>{source}</pre> : <p className="dl-tool-note">{readableSummary(result.summary) || t("tool_details.page_action_completed")}</p>}</div>;
  }
  const text = [payload.message, payload.description, payload.text, payload.content].map(value => readableSummary(asText(value))).find(Boolean);
  const fields = text ? Object.fromEntries(Object.entries(payload).filter(([key]) => !["message","description","text","content"].includes(key))) : payload;
  return <div className="dl-tool-note">{text ? <Markdown content={text} /> : <p>{readableSummary(result.summary) || t("tool_details.action_completed")}</p>}<ToolParameters value={domainFields(fields)} /></div>;
}

export function ApprovalDetails({ result, requested }: { result: ToolResult["arguments"]; requested: { readable_roots: string[]; writable_roots: string[]; operation_details?: ToolResult["arguments"] } }) {
  return <div className="dl-approval-details"><ToolParameters value={result} />{requested.readable_roots.length > 0 && <section className="dl-permission-roots"><h4>{t("tool_details.additional_read_access")}</h4><ul>{requested.readable_roots.map(path => <li key={path}><code>{path}</code></li>)}</ul></section>}{requested.writable_roots.length > 0 && <section className="dl-permission-roots"><h4>{t("tool_details.additional_write_access")}</h4><ul>{requested.writable_roots.map(path => <li key={path}><code>{path}</code></li>)}</ul></section>}{requested.operation_details != null && <section className="dl-permission-roots"><h4>{t("tool_details.execution_details")}</h4><ToolParameters value={requested.operation_details} /></section>}</div>;
}
