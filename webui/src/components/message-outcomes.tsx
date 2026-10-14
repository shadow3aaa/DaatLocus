import { t } from "@/lib/i18n";
import { useEffect, useRef, useState } from "react";
import { ChevronDownIcon, ExternalLinkIcon, FileDiffIcon, FileTextIcon, ImageIcon } from "lucide-react";
import type { ToolSummary, ToolResult } from "@/lib/actor-api";
import { getToolResult } from "@/lib/actor-api";
import { asObject, asText, domainToolName } from "@/lib/tool-presentation";
import { safeLink } from "@/lib/conversation-links";
import { ToolDetail } from "@/components/tool-details";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogHeader, DialogTitle, DialogDescription } from "@/components/ui/dialog";
import { Skeleton } from "@/components/ui/skeleton";

export function MessageOutcomes({ actorId, tools, onActor }: { actorId: string; tools: ToolSummary[]; onActor(id: string): void }) {
  const [more, setMore] = useState(false); const [view, setView] = useState(false); const [selected, setSelected] = useState<ToolSummary | null>(null); const [detail, setDetail] = useState<ToolResult | null>(null); const [error, setError] = useState("");
  const request = useRef<AbortController | null>(null);
  useEffect(() => () => request.current?.abort(), [actorId]);
  const artifacts = tools.filter(tool => !tool.error && domainToolName(tool.name) === "artifacts__present" && asText(asObject(tool.outcome).uri));
  const changes = tools.filter(tool => !tool.error && ["files__write_file", "files__edit_file", "files__copy_file"].includes(domainToolName(tool.name)) && asText(asObject(tool.outcome).path));
  const files = new Map<string, ToolSummary[]>();
  for (const change of changes) { const path = asText(asObject(change.outcome).path); files.set(path, [...files.get(path) || [], change]); }
  const count = (field: string, items = changes) => items.reduce((total, tool) => total + Number(asObject(tool.outcome)[field] || 0), 0);
  const hasStats = changes.some(tool => typeof asObject(tool.outcome).added_lines === "number");
  async function inspect(tool: ToolSummary) { request.current?.abort(); const abort = new AbortController(); request.current = abort; setSelected(tool); setDetail(null); setError(""); setView(true); try { const result = await getToolResult(actorId, tool.id, {signal:abort.signal}); if (!abort.signal.aborted) setDetail(result); } catch { if (!abort.signal.aborted) setError(t("message_outcomes.could_not_load_changes")); } }
  return <div className="dl-message-outcomes">{artifacts.map(tool => { const info = asObject(tool.outcome); const href = safeLink(asText(info.uri)); const Icon = info.kind === "image" || info.kind === "svg" ? ImageIcon : FileTextIcon; return href && <a key={tool.id} className="dl-outcome-artifact" href={href} target="_blank" rel="noreferrer"><Icon aria-hidden="true" /><span>{asText(info.title) || t("message_outcomes.delivered_files")}<small>{asText(info.description)}</small></span><ExternalLinkIcon aria-hidden="true" /></a>; })}
    {files.size > 0 && <section className="dl-changed-files"><header><FileDiffIcon aria-hidden="true" /><div><strong>{t("message_outcomes.updated_files", {count:files.size})}</strong>{hasStats && <p><span className="dl-added-count">+{count("added_lines")}</span> <span className="dl-removed-count">−{count("removed_lines")}</span>{changes.length !== files.size && <small> · {t("message_outcomes.change_count", {count:changes.length})}</small>}</p>}</div><Button variant="ghost" size="sm" onClick={() => { void inspect(changes[0]); }}>{t("message_outcomes.view_changes")}</Button></header><ul>{Array.from(files).slice(0, more ? undefined : 3).map(([path, items]) => <li key={path}><button type="button" onClick={() => { void inspect(items.at(-1)!); }}>{path}</button>{items.some(item => typeof asObject(item.outcome).added_lines === "number") ? <small><span className="dl-added-count">+{count("added_lines", items)}</span> <span className="dl-removed-count">−{count("removed_lines", items)}</span></small> : <small>{t("message_outcomes.written")}</small>}</li>)}</ul>{files.size > 3 && !more && <button className="dl-changes-more" type="button" onClick={() => setMore(true)}>{t("message_outcomes.more_files", {count:files.size-3})}<ChevronDownIcon aria-hidden="true" /></button>}</section>}
    <Dialog open={view} onOpenChange={setView}><DialogContent className="dl-change-dialog"><DialogHeader><DialogTitle>{t("message_outcomes.file_changes")}</DialogTitle><DialogDescription className="sr-only">{selected && asText(asObject(selected.outcome).path)}</DialogDescription></DialogHeader>{changes.length > 1 && <div className="dl-change-picker">{changes.map(tool => <Button key={tool.id} variant={selected?.id === tool.id ? "secondary" : "ghost"} size="sm" onClick={() => { void inspect(tool); }}>{asText(asObject(tool.outcome).path)}</Button>)}</div>}<div className="dl-change-content">{detail ? <ToolDetail result={detail} onActor={onActor} /> : error ? <div className="dl-client-error" role="alert"><span>{error}</span><Button variant="ghost" size="sm" onClick={() => { if (selected) void inspect(selected); }}>{t("app.retry")}</Button></div> : <Skeleton className="h-28 w-full" />}</div></DialogContent></Dialog>
  </div>;
}
