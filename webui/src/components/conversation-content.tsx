import { resourceSource } from "@/lib/transport";
import { t } from "@/lib/i18n";
import { useEffect, useId, useState } from "react";
import { CheckIcon, ChevronRightIcon, CopyIcon, DownloadIcon } from "lucide-react";
import { Button } from "@/components/ui/button";
import { type Attachment, type ToolSummary } from "@/lib/actor-api";
import { ToolActivityRow } from "@/components/tool-activity-row";
import { asObject as object, asText as string, toolIcon, activitySummary, activityIcon, activityLabel, operationToolName, domainToolName, approvalNotice } from "@/lib/tool-presentation";


export function AttachmentList({ items }: { items: Attachment[] }) {
  return <div className="dl-message-attachments">{items.map(item => item.media_type.startsWith("image/") ? <a href={item.uri} target="_blank" rel="noreferrer" key={item.id}><img src={resourceSource(item.uri)} alt={item.name} loading="lazy" /></a> : <a className="dl-file-reference" href={item.uri} download={item.name} key={item.id}><DownloadIcon aria-hidden="true" /><span>{item.name}</span><small>{formatBytes(item.size_bytes)}</small></a>)}</div>;
}
function formatBytes(bytes: number) { return bytes > 1024 * 1024 ? `${(bytes / 1024 / 1024).toFixed(1)} MB` : `${Math.max(1, Math.round(bytes / 1024))} KB`; }

function ToolGroupIcon() {
  return <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><circle cx="6" cy="7" r="3" /><circle cx="18" cy="5" r="3" /><circle cx="6" cy="19" r="3" /><circle cx="18" cy="17" r="3" /><path d="M9 7h2a4 4 0 0 0 4-2M9 19h2a4 4 0 0 0 4-4v-2a4 4 0 0 1 3-4" /></svg>;
}

export function Activities({ actorId, tools, onActor, thinking }: { actorId: string; tools: (ToolSummary & { status?: "running" | "completed" | "unknown" })[]; thinking?: string | null; onActor(id: string): void }) {
  const contentId = useId();
  const [open, setOpen] = useState(false);
  const names = tools.filter(tool => !approvalNotice(tool.outcome)).map(operationToolName);
  const domains = names.map(domainToolName);
  const activeCommand = tools.filter(tool => tool.status === "running" && operationToolName(tool).startsWith("terminal__") && string(object(tool.outcome).command)).at(-1);
  const commandPreview = activeCommand ? activityLabel(activeCommand) : null;
  const GroupIcon = activeCommand ? toolIcon(activeCommand.name, activeCommand.outcome) : domains.some(name => name.startsWith("web__")) && domains.some(name => !name.startsWith("web__")) ? ToolGroupIcon : activityIcon(names);
  if (!tools.length) return thinking ? <div className="dl-work-final dl-work-actor-reply"><div className="dl-bubble dl-bubble-actor" role="status">{thinking}</div></div> : null;
  return <div className="dl-activities">
    <button type="button" className={thinking ? "dl-activity-heading dl-activity-thinking" : "dl-activity-heading"} data-running-command={!thinking && !open && commandPreview ? true : undefined} aria-expanded={open} aria-controls={contentId} onClick={() => setOpen(!open)}>
      {!thinking && <GroupIcon aria-hidden="true" />}
      <span className="dl-activity-title" title={!thinking && !open ? commandPreview?.detail : undefined}>{thinking || (commandPreview ? open ? t("work_activity.activity") : `${commandPreview.label} ${commandPreview.detail}` : names.length ? activitySummary(names) : t("tool_details.permission_status"))}</span>
      <ChevronRightIcon aria-hidden="true" className={open ? "dl-activity-open" : undefined} />
    </button>
    {open && <div id={contentId} className="dl-step-list" role="group" aria-label={t("work_activity.activity")}>
      {tools.map(tool=><ToolActivityRow key={tool.call_id} actorId={actorId} tool={tool} onActor={onActor}/>)}
    </div>}
  </div>;
}

export function CopyMessage({ content }: { content: string }) {
  const [copied, setCopied] = useState(false);
  const [failed, setFailed] = useState(false);
  useEffect(() => { if (!copied && !failed) return; const timer = setTimeout(() => { setCopied(false); setFailed(false); }, 2000); return () => clearTimeout(timer); }, [copied, failed]);
  return <Button variant="ghost" size={failed ? "sm" : "icon-xs"} className="dl-copy-message" aria-label={t("conversation_content.copy_message")} onClick={() => {
    setFailed(false);
    void navigator.clipboard.writeText(content).then(() => setCopied(true)).catch(() => setFailed(true));
  }}>{copied ? <CheckIcon /> : <CopyIcon />}{failed && <span role="status">{t("conversation_content.copy_failed")}</span>}</Button>;
}
