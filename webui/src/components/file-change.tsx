import { t } from "@/lib/i18n";
import { useEffect, useState } from "react";
import { CheckIcon, CopyIcon } from "lucide-react";
import { Button } from "@/components/ui/button";
import type { JsonValue } from "@/lib/actor-api";
import { asObject, asRows, asText } from "@/lib/tool-presentation";
import { cn } from "@/lib/utils";

export function FileChangeView({ path, value }: { path: string; value: JsonValue | undefined }) {
  const change = asObject(value);
  const lines = asRows(change.lines);
  const [limit, setLimit] = useState(60);
  const [copied, setCopied] = useState(false);
  const [copyFailed, setCopyFailed] = useState(false);
  useEffect(() => { setLimit(60); setCopied(false); setCopyFailed(false); }, [path, value]);
  useEffect(() => { if (!copied) return; const timer = setTimeout(() => setCopied(false), 1600); return () => clearTimeout(timer); }, [copied]);
  if (!value) return <div className="dl-file-details"><code>{path}</code><p className="dl-tool-note">{t("file_change.file_updated_no_text_diff_is_available")}</p></div>;
  return <section className="dl-inline-diff"><header><code>{path}</code><small><span className="dl-added-count">+{Number(change.added_lines || 0)}</span> <span className="dl-removed-count">−{Number(change.removed_lines || 0)}</span></small><Button variant="ghost" size="icon-xs" aria-label={t("file_change.copy_changes")} onClick={() => { setCopyFailed(false); void navigator.clipboard.writeText(asText(change.patch)).then(() => setCopied(true)).catch(() => setCopyFailed(true)); }}>{copied ? <CheckIcon /> : <CopyIcon />}</Button></header>
    {copyFailed && <p role="alert" className="dl-operation-error">{t("conversation_content.copy_failed")}</p>}
    <div className="dl-diff-lines" role="region" aria-label={t("file_change.changes_to_value0", {value0: path})} tabIndex={0}>{lines.slice(0,limit).map((line, index) => <div key={index} className={cn("dl-diff-line", line.kind === "add" && "dl-diff-line-added", line.kind === "remove" && "dl-diff-line-removed")}><span className="dl-diff-number">{typeof line.old_line === "number" ? line.old_line : ""}</span><span className="dl-diff-number">{typeof line.new_line === "number" ? line.new_line : ""}</span><span className="dl-diff-sign">{line.kind === "add" ? "+" : line.kind === "remove" ? "−" : " "}</span><code>{asText(line.text)}</code></div>)}{!lines.length && <p className="dl-tool-note">{t("file_change.no_text_changes")}</p>}</div>
    {lines.length > limit && <Button variant="ghost" size="sm" onClick={() => setLimit(old => old + 100)}>{t("file_change.more_lines", {count:Math.min(100,lines.length-limit)})}</Button>}
  </section>;
}
