import { t } from "@/lib/i18n";
import { useEffect, useRef, useState } from "react";
import { ExternalLinkIcon, FileDiffIcon, FileTextIcon, ImageIcon } from "lucide-react";
import type { ToolSummary, ToolResult } from "@/lib/actor-api";
import { getToolResult } from "@/lib/actor-api";
import { asObject, asText, operationToolName } from "@/lib/tool-presentation";
import { safeLink } from "@/lib/conversation-links";
import { ToolDetail } from "@/components/tool-details";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogHeader, DialogTitle, DialogDescription } from "@/components/ui/dialog";
import { Skeleton } from "@/components/ui/skeleton";
import { LazyPage } from "@/components/lazy-page";

export function messageOutcomeTools(tools: ToolSummary[]) {
  return {
    artifacts: tools.filter(tool => !tool.error && operationToolName(tool) === "artifacts__present" && safeLink(asText(asObject(tool.outcome).uri))),
    changes: tools.filter(tool => !tool.error && ["files__write_file", "files__edit_file", "files__copy_file"].includes(operationToolName(tool)) && asText(asObject(tool.outcome).path)),
  };
}

export function MessageOutcomes({ actorId, tools, onActor }: { actorId: string; tools: ToolSummary[]; onActor(id: string): void }) {
  const [limit, setLimit] = useState(3); const [view, setView] = useState(false); const [selected, setSelected] = useState<ToolSummary | null>(null); const [detail, setDetail] = useState<ToolResult | null>(null); const [error, setError] = useState("");
  const request = useRef<AbortController | null>(null);
  useEffect(() => () => request.current?.abort(), [actorId]);
  const { artifacts, changes } = messageOutcomeTools(tools);
  const files = new Map<string, ToolSummary[]>();
  for (const change of changes) { const path = asText(asObject(change.outcome).path); files.set(path, [...files.get(path) || [], change]); }
  const count = (field: string, items = changes) => items.reduce((total, tool) => total + Number(asObject(tool.outcome)[field] || 0), 0);
  const hasStats = changes.some(tool => typeof asObject(tool.outcome).added_lines === "number");
  async function inspect(tool: ToolSummary) { request.current?.abort(); const abort = new AbortController(); request.current = abort; setSelected(tool); setDetail(null); setError(""); setView(true); try { const result = await getToolResult(actorId, tool.id, {signal:abort.signal}); if (!abort.signal.aborted) setDetail(result); } catch { if (!abort.signal.aborted) setError(t("message_outcomes.could_not_load_changes")); } }
  return <div className="dl-message-outcomes">{artifacts.map(tool => { const info = asObject(tool.outcome); const href = safeLink(asText(info.uri)); const Icon = info.kind === "image" || info.kind === "svg" ? ImageIcon : FileTextIcon; return href && <a key={tool.id} className="dl-outcome-artifact" href={href} target="_blank" rel="noreferrer"><Icon aria-hidden="true" /><span>{asText(info.title) || t("message_outcomes.delivered_files")}<small>{asText(info.description)}</small></span><ExternalLinkIcon aria-hidden="true" /></a>; })}
    {files.size > 0 && <><header className="dl-changed-files-heading"><FileDiffIcon aria-hidden="true" /><div><strong>{t("message_outcomes.updated_files", {count:files.size})}</strong>{hasStats && <p><span className="dl-added-count">+{count("added_lines")}</span> <span className="dl-removed-count">−{count("removed_lines")}</span>{changes.length !== files.size && <small> · {t("message_outcomes.change_count", {count:changes.length})}</small>}</p>}</div><Button variant="ghost" size="sm" onClick={() => { void inspect(changes[0]); }}>{t("message_outcomes.view_changes")}</Button></header><ul className="dl-changed-files-list">{Array.from(files).slice(0, limit).map(([path, items]) => <li key={path}><button type="button" onClick={() => { void inspect(items.at(-1)!); }}>{path}</button>{items.some(item => typeof asObject(item.outcome).added_lines === "number") ? <small><span className="dl-added-count">+{count("added_lines", items)}</span> <span className="dl-removed-count">−{count("removed_lines", items)}</span></small> : <small>{t("message_outcomes.written")}</small>}</li>)}</ul>{files.size > limit && <LazyPage cursor={limit} onLoad={async () => setLimit(old => old + 30)} errorMessage={t("message_outcomes.could_not_load_changes")} />}</>}
    <Dialog open={view} onOpenChange={setView}><DialogContent className="dl-change-dialog">
      <DialogHeader><DialogTitle>{t("message_outcomes.file_changes")}</DialogTitle><DialogDescription className="sr-only">{selected && asText(asObject(selected.outcome).path)}</DialogDescription></DialogHeader>
      {changes.length > 1 && <div className="dl-change-picker">{changes.map(tool => {
        const path = asText(asObject(tool.outcome).path), versions = files.get(path)!;
        return <Button key={tool.id} title={path} variant={selected?.id === tool.id ? "secondary" : "ghost"} size="sm" onClick={() => { void inspect(tool); }}><span className="truncate">{path.replace(/\\/g,"/").split("/").at(-1)}</span>{versions.length > 1 && <span>{versions.indexOf(tool) + 1}/{versions.length}</span>}</Button>;
      })}</div>}
      <div className="dl-change-content">{detail ? <ToolDetail result={detail} onActor={onActor} /> : error ? <div className="dl-client-error" role="alert"><span>{error}</span><Button variant="ghost" size="sm" onClick={() => { if (selected) void inspect(selected); }}>{t("app.retry")}</Button></div> : <Skeleton className="h-28 w-full" />}</div>
    </DialogContent></Dialog>
  </div>;
}
