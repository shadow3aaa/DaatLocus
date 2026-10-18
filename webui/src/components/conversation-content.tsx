import { motionScrollBehavior } from "@/lib/appearance";
import { resourceSource } from "@/lib/transport";
import { t } from "@/lib/i18n";
import { useEffect, useId, useRef, useState } from "react";
import { CheckIcon, ChevronRightIcon, CopyIcon, DownloadIcon, MessageCircleIcon, ShieldCheckIcon } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { LazyPage } from "@/components/lazy-page";
import { localActorId } from "@/lib/conversation-links";
import { getActor, getMessage, listMessages, subscribeActorEvents, getToolResult, resolveApproval, type ApprovalRecord, type Attachment, type ToolResult, type ToolSummary, type MessageStatus } from "@/lib/actor-api";
import { ToolDetail, ApprovalDetails } from "@/components/tool-details";
import { asObject as object, asText as string, readableSummary, toolLabel, toolIcon, activitySummary, activityIcon, activityLabel, domainToolName } from "@/lib/tool-presentation";


export function ActorReference({ actorId, label, onOpen, detail, messageId }: { actorId: string; label?: string; onOpen(id: string, messageId?: string): void; detail?: string; messageId?: string }) {
  const targetId = localActorId(actorId);
  const [name, setName] = useState(label || t("actor_dialog.actor_conversation"));
  const [status, setStatus] = useState<MessageStatus | "ready" | "unknown">("unknown");
  useEffect(() => {
    const controller = new AbortController(); setStatus("unknown"); setName(label || t("actor_dialog.actor_conversation"));
    void (async () => {
      const page = messageId ? await listMessages(targetId, { limit: 1, signal: controller.signal }) : null;
      const actor = await getActor(targetId, { signal: controller.signal });
      if (controller.signal.aborted) return;
      setName(label || actor.config?.name || actor.name || t("actor_dialog.actor_conversation"));
      if (!messageId) { setStatus("ready"); return; }
      const task = await getMessage(targetId, messageId, { signal: controller.signal });
      if (controller.signal.aborted) return;
      setStatus(task.status);
      if (!["queued", "running"].includes(task.status)) return;
      await subscribeActorEvents(targetId, { after: page?.event_cursor || 0, signal: controller.signal, onEvent: async record => {
        const event = record.event;
        if (event.type !== "message_updated" || event.message_id !== messageId) return;
        setStatus(event.status);
        if (!["queued", "running"].includes(event.status)) controller.abort();
      } });
    })().catch(() => {});
    return () => controller.abort();
  }, [targetId, label, messageId]);
  const stateLabel = { queued: t("conversation_content.queued"), running: t("conversation_content.in_progress"), completed: t("conversation_content.completed"), failed: t("conversation_content.incomplete"), cancelled: t("conversation_content.stopped"), interrupted: t("conversation_content.interrupted"), ready: t("conversation_content.ready"), unknown: t("conversation_content.view_conversation") }[status];
  return <button type="button" className="dl-actor-reference" data-status={status} onClick={() => onOpen(actorId, messageId)} aria-label={t("conversation_content.view_value0_value1", {value0: name, value1: stateLabel})}>
    <MessageCircleIcon aria-hidden="true" /><span><strong>{name}</strong>{detail && <small>{detail}</small>}</span><span className="dl-reference-action"><span>{stateLabel}</span><i className="dl-task-state" aria-hidden="true" /></span>
  </button>;
}

export function AttachmentList({ items }: { items: Attachment[] }) {
  return <div className="dl-message-attachments">{items.map(item => item.media_type.startsWith("image/") ? <a href={item.uri} target="_blank" rel="noreferrer" key={item.id}><img src={resourceSource(item.uri)} alt={item.name} loading="lazy" /></a> : <a className="dl-file-reference" href={item.uri} download={item.name} key={item.id}><DownloadIcon aria-hidden="true" /><span>{item.name}</span><small>{formatBytes(item.size_bytes)}</small></a>)}</div>;
}
function formatBytes(bytes: number) { return bytes > 1024 * 1024 ? `${(bytes / 1024 / 1024).toFixed(1)} MB` : `${Math.max(1, Math.round(bytes / 1024))} KB`; }

function ActorToolReference({ actorId, tool, onActor }: { actorId: string; tool: ToolSummary; onActor(id: string): void }) {
  const [result, setResult] = useState<ToolResult | null>(null);
  const [failed, setFailed] = useState(false);
  useEffect(() => { const controller = new AbortController(); void getToolResult(actorId, tool.id, { signal: controller.signal }).then(setResult).catch(error => { if (error?.name !== "AbortError") setFailed(true); }); return () => controller.abort(); }, [actorId, tool.id]);
  if (failed) return <span className="dl-step-label">{toolLabel(tool.name)}</span>;
  if (!result) return <Skeleton className="h-12 w-64 rounded-xl" />;
  const payload = object(result.payload);
  const target = string(payload.actor_id) || string(payload.id);
  return target ? <ActorReference actorId={target} label={string(payload.name)} messageId={string(payload.message_id)} onOpen={onActor} detail={tool.name === "actors__send" ? t("conversation_content.delegated") : t("conversation_content.created")} /> : <span className="dl-step-label">{readableSummary(tool.summary) || toolLabel(tool.name)}</span>;
}

function ToolGroupIcon() {
  return <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><circle cx="6" cy="7" r="3" /><circle cx="18" cy="5" r="3" /><circle cx="6" cy="19" r="3" /><circle cx="18" cy="17" r="3" /><path d="M9 7h2a4 4 0 0 0 4-2M9 19h2a4 4 0 0 0 4-4v-2a4 4 0 0 1 3-4" /></svg>;
}

export function Activities({ actorId, tools, onActor, thinking, nextCursor, onLoadMore }: { actorId: string; tools: (ToolSummary & { status?: "running" | "completed" | "unknown" })[]; thinking?: string | null; onActor(id: string): void; nextCursor?: number | null; onLoadMore?(): Promise<unknown> }) {
  const contentId = useId();
  const [open, setOpen] = useState(false);
  const [expanded, setExpanded] = useState<string | null>(null);
  const [details, setDetails] = useState<Record<string, ToolResult>>({});
  const [error, setError] = useState("");
  const pendingDetail = useRef<AbortController | null>(null);
  useEffect(() => () => pendingDetail.current?.abort(), [actorId]);
  const failures = tools.filter(tool => tool.error).length;
  const actorRef = (tool: ToolSummary & { status?: string }) => !tool.id.startsWith("live:") && tool.status !== "running" && !tool.error && ["actors__create", "actors__send"].includes(domainToolName(tool.name));
  const names = tools.map(tool => tool.name);
  const domains = names.map(domainToolName);
  const GroupIcon = domains.some(name => name.startsWith("browser__")) && domains.some(name => !name.startsWith("browser__")) ? ToolGroupIcon : activityIcon(names);
  if (!tools.length) return thinking ? <div className="dl-activities"><div className="dl-activity-heading dl-activity-thinking" role="status"><span className="dl-activity-title">{thinking}</span></div></div> : null;
  async function expand(tool: ToolSummary, retry = false) {
    pendingDetail.current?.abort();
    if (expanded === tool.id && !retry) { setExpanded(null); return; }
    setExpanded(tool.id); setError("");
    const controller = new AbortController(); pendingDetail.current = controller;
    if (!details[tool.id]) try { const result = await getToolResult(actorId, tool.id, {signal: controller.signal}); if (!controller.signal.aborted) setDetails(old => ({ ...old, [tool.id]: result })); } catch { if (!controller.signal.aborted) setError(t("conversation_content.could_not_load_details_try_again")); }
  }
  return <div className="dl-activities">
    <button type="button" className={thinking ? "dl-activity-heading dl-activity-thinking" : "dl-activity-heading"} aria-expanded={open} aria-controls={contentId} onClick={() => setOpen(!open)}>
      {!thinking && <GroupIcon aria-hidden="true" />}
      <span className="dl-activity-title">{thinking || activitySummary(tools.map(tool => tool.name))}</span>
      {failures > 0 && <span className="dl-step-error-state">{failures} {t("conversation_content.incomplete")}</span>}
      <ChevronRightIcon aria-hidden="true" className={open ? "dl-activity-open" : undefined} />
    </button>
    {open && <div id={contentId} className="dl-step-list" role="group" aria-label={t("work_round.activity")}>
      {tools.map(tool => {
        const Icon = toolIcon(tool.name, tool.outcome);
        const text = activityLabel(tool);
        const outcome = object(tool.outcome);
        const domain = domainToolName(tool.name);
        const changed = ["files__write_file", "files__edit_file", "files__copy_file"].includes(domain);
        const selected = expanded === tool.id;
        const expandedLabel = changed ? t("tool_presentation.edited_file_details") : domain.startsWith("terminal__") ? t("tool_presentation.command_ran") : text.label;
        return actorRef(tool) ? <ActorToolReference key={tool.id} actorId={actorId} tool={tool} onActor={onActor} /> : <div key={tool.id} className="dl-step">
          <button type="button" className="dl-step-label" disabled={tool.status === "running" || tool.id.startsWith("live:")} onClick={() => { void expand(tool); }} aria-expanded={selected}>
            <Icon aria-hidden="true" /><span className="dl-step-text">{selected ? expandedLabel : text.label}{!selected && text.detail && <> <span className={changed ? "dl-step-summary dl-step-file" : "dl-step-summary"} title={text.detail}>{changed ? text.detail.replace(/\\/g,"/").split("/").at(-1) : text.detail}</span></>}</span>
            {!selected && changed && typeof outcome.added_lines === "number" && <span className="dl-step-stat">+{outcome.added_lines}</span>}
            {!selected && changed && typeof outcome.removed_lines === "number" && <span className="dl-step-stat">−{outcome.removed_lines}</span>}
            {tool.error && <span className="dl-step-error-state">{t("conversation_content.failed")}</span>}
            <ChevronRightIcon aria-hidden="true" className={selected ? "dl-step-chevron dl-activity-open" : "dl-step-chevron"} />
          </button>
          {selected && <div className="dl-step-detail">{details[tool.id] ? <ToolDetail result={details[tool.id]} onActor={onActor} /> : error ? <div className="dl-client-error" role="alert"><span>{error}</span><Button variant="ghost" size="sm" onClick={() => { void expand(tool, true); }}>{t("app.retry")}</Button></div> : <Skeleton className="h-16 w-full rounded-lg" />}</div>}
        </div>;
      })}
      {nextCursor != null && onLoadMore && <LazyPage cursor={nextCursor} onLoad={onLoadMore} errorMessage={t("conversation_content.could_not_load_earlier_actions")} />}
    </div>}
  </div>;
}

export function Approval({ record, canManage, onResolved }: { record: ApprovalRecord; canManage: boolean; onResolved(): void }) {
  const [busy, setBusy] = useState(false); const [error, setError] = useState(""); const [open, setOpen] = useState(false);
  const actionsRef = useRef<HTMLDivElement | null>(null);
  useEffect(() => { if (open) actionsRef.current?.scrollIntoView({ block: "nearest", behavior: motionScrollBehavior() }); }, [open]);
  const awaiting = record.decision === "pending" && record.mode === "ask";
  const explanation = record.mode === "ask" ? record.decision === "pending" ? canManage ? t("conversation_content.review_the_action_and_requested_access") : t("conversation_content.waiting_for_the_owner_to_approve") : record.decision === "approved" ? t("conversation_content.this_action_was_approved") : record.decision === "denied" ? t("conversation_content.this_action_was_denied") : t("conversation_content.this_action_was_cancelled") : readableSummary(record.reason) || t("conversation_content.reviewing_this_action");
  async function decide(decision: "approve" | "deny") { setBusy(true); setError(""); try { await resolveApproval(record.id, decision); onResolved(); } catch { setError(t("conversation_content.approval_status_changed_refresh_and_try_again")); } finally { setBusy(false); } }
  return <section className="dl-approval"><div className="dl-approval-heading"><ShieldCheckIcon aria-hidden="true" /><span>{awaiting ? t("conversation_content.waiting_for_your_approval") : record.mode === "auto" && record.decision === "pending" ? t("conversation_content.reviewing") : record.decision === "approved" ? t("conversation_content.approved") : record.decision === "denied" ? t("conversation_content.action_denied") : t("conversation_content.approval_cancelled")}</span></div><p>{explanation}</p>
    <button type="button" className="dl-disclosure" onClick={() => setOpen(!open)} aria-expanded={open}>{t("conversation_content.view_action_and_requested_access")}<ChevronRightIcon aria-hidden="true" className={open ? "dl-chevron-open" : undefined} /></button>
    {open && <><p className="dl-approval-operation">{toolLabel(record.call.name)}</p><ApprovalDetails result={record.call.arguments} requested={record.requested} /></>}
    {awaiting && canManage && <div className="dl-approval-actions" ref={actionsRef}><Button size="sm" variant="outline" disabled={busy} onClick={() => { void decide("deny"); }}>{t("conversation_content.deny")}</Button><Button size="sm" disabled={busy} onClick={() => { void decide("approve"); }}>{t("conversation_content.approve_action")}</Button></div>}{awaiting && !canManage && <small>{t("conversation_content.only_the_owner_can_approve_this_action")}</small>}{error && <p role="alert">{error}</p>}
  </section>;
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
