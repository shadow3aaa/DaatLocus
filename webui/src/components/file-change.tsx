import { t } from "@/lib/i18n";
import { useEffect, useState } from "react";
import { CheckIcon, CopyIcon } from "lucide-react";
import { Button } from "@/components/ui/button";
import type { JsonValue } from "@/lib/actor-api";
import { asObject, asRows, asText } from "@/lib/tool-presentation";
import { cn } from "@/lib/utils";
import { useCodeHighlight } from "@/hooks/use-code-highlight";
import type { ShikiHighlightToken } from "@/lib/shiki-highlight";

function changedRange(text: string, other: string): [number, number] {
  const a = Array.from(text), b = Array.from(other);
  let prefix = 0, suffix = 0;
  while (prefix < a.length && prefix < b.length && a[prefix] === b[prefix]) prefix++;
  while (suffix < a.length-prefix && suffix < b.length-prefix && a[a.length-1-suffix] === b[b.length-1-suffix]) suffix++;
  return [a.slice(0,prefix).join("").length, text.length-a.slice(a.length-suffix).join("").length];
}
function sourceTokens(text: string, tokens: ShikiHighlightToken[] | undefined, changed?: [number, number]) {
  let offset = 0;
  return (tokens?.length ? tokens : [{content:text}]).map((token,index) => {
    const start = offset; offset += token.content.length;
    const from = changed ? Math.max(0,changed[0]-start) : token.content.length;
    const to = changed ? Math.min(token.content.length,changed[1]-start) : 0;
    return <span key={index} style={{color:token.color}}>{from < to ? <>{token.content.slice(0,from)}<mark className="dl-diff-word">{token.content.slice(from,to)}</mark>{token.content.slice(to)}</> : token.content}</span>;
  });
}

export function FileChangeView({ path, value }: { path: string; value: JsonValue | undefined }) {
  const change = asObject(value);
  const lines = asRows(change.lines);
  const highlight = useCodeHighlight(lines.map(line => asText(line.text)).join("\n"), path);
  const [limit, setLimit] = useState(60);
  const [copied, setCopied] = useState(false);
  const [copyFailed, setCopyFailed] = useState(false);
  useEffect(() => { setLimit(60); setCopied(false); setCopyFailed(false); }, [path, value]);
  useEffect(() => { if (!copied) return; const timer = setTimeout(() => setCopied(false), 1600); return () => clearTimeout(timer); }, [copied]);
  if (!value) return <div className="dl-file-details"><code>{path}</code><p className="dl-tool-note">{t("file_change.file_updated_no_text_diff_is_available")}</p></div>;
  const filename = path.replace(/\\/g,"/").split("/").at(-1) || path;
  const patch = asText(change.patch) || lines.map(line => `${line.kind === "add" ? "+" : line.kind === "remove" ? "-" : " "}${asText(line.text)}`).join("\n");
  return <section className="dl-inline-diff"><header><span title={path}>{filename}</span><small><span className="dl-added-count">+{Number(change.added_lines || 0)}</span> <span className="dl-removed-count">−{Number(change.removed_lines || 0)}</span></small><Button variant="ghost" size="icon-xs" aria-label={t("file_change.copy_changes")} onClick={() => { setCopyFailed(false); void navigator.clipboard.writeText(patch).then(() => setCopied(true)).catch(() => setCopyFailed(true)); }}>{copied ? <CheckIcon /> : <CopyIcon />}</Button></header>
    {copyFailed && <p role="alert" className="dl-operation-error">{t("conversation_content.copy_failed")}</p>}
    <div className="dl-diff-lines" role="region" aria-label={t("file_change.changes_to_value0", {value0: path})} tabIndex={0}>{lines.slice(0,limit).map((line, index) => {
      const paired = line.kind === "remove" && lines[index+1]?.kind === "add" ? lines[index+1] : line.kind === "add" && lines[index-1]?.kind === "remove" ? lines[index-1] : null;
      const text = asText(line.text);
      return <div key={index} className={cn("dl-diff-line", line.kind === "add" && "dl-diff-line-added", line.kind === "remove" && "dl-diff-line-removed")}><span className="dl-diff-number">{line.kind === "add" ? line.new_line as number : (line.old_line ?? line.new_line) as number}</span><span className="sr-only">{line.kind === "add" ? "+" : line.kind === "remove" ? "−" : " "}</span><code>{sourceTokens(text,highlight?.lines[index],paired ? changedRange(text,asText(paired.text)) : undefined)}</code></div>;
    })}{!lines.length && <p className="dl-tool-note">{t("file_change.no_text_changes")}</p>}</div>
    {lines.length > limit && <Button variant="ghost" size="sm" onClick={() => setLimit(old => old + 100)}>{t("file_change.more_lines", {count:Math.min(100,lines.length-limit)})}</Button>}
  </section>;
}
