import { isCodeTheme } from "@/lib/appearance";
import { resourceSource } from "@/lib/transport";
import { t } from "@/lib/i18n";
import { memo, useEffect, useState, type ReactNode } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { CheckIcon, CopyIcon, MessageCircleIcon } from "lucide-react";
import { Button } from "@/components/ui/button";
import { actorIdFromUrl, resourceActorId, safeLink } from "@/lib/conversation-links";
import { highlightCodeWithShiki, type ShikiHighlightedCode } from "@/lib/shiki-highlight";

export function CodeBlock({ code, language, startLine }: { code: string; language: string; startLine?: number }) {
  const [copied, setCopied] = useState(false);
  const [copyFailed, setCopyFailed] = useState(false);
  const [highlight, setHighlight] = useState<ShikiHighlightedCode | null>(null);
  useEffect(() => {
    let live = true;
    let revision = 0;
    const update = () => { const current = ++revision; void highlightCodeWithShiki(code, language, document.documentElement.classList.contains("dark") ? "dark" : "light", isCodeTheme(document.documentElement.dataset.codeTheme) ? document.documentElement.dataset.codeTheme : "github").then(value => { if (live && current === revision) setHighlight(value); }).catch(() => { if (live && current === revision) setHighlight(null); }); };
    update();
    const observer = new MutationObserver(update);
    observer.observe(document.documentElement, { attributes: true, attributeFilter: ["class", "data-code-theme"] });
    return () => { live = false; observer.disconnect(); };
  }, [code, language]);
  useEffect(() => { if (!copied) return; const timer = setTimeout(() => setCopied(false), 1600); return () => clearTimeout(timer); }, [copied]);
  return <div className="dl-code-block">
    <div className="dl-code-heading"><span>{language || t("markdown.code")}</span><Button variant="ghost" size="sm" aria-label={t("markdown.copy_code")} onClick={() => { setCopyFailed(false); void navigator.clipboard.writeText(code).then(() => setCopied(true)).catch(() => setCopyFailed(true)); }}>{copied ? <CheckIcon data-icon="inline-start" /> : <CopyIcon data-icon="inline-start" />}{copied ? t("markdown.copied") : t("markdown.copy")}</Button></div>
    {copyFailed && <p role="alert" className="dl-operation-error">{t("conversation_content.copy_failed")}</p>}
    <pre style={{background: highlight?.background, color: highlight?.foreground}} className={startLine != null ? "dl-numbered-source" : undefined}><code>{startLine != null ? (code.endsWith("\n") ? code.slice(0,-1) : code).split("\n").map((line,index) => <span className="dl-source-line" key={index}><span className="dl-source-number">{startLine + index}</span><span>{highlight?.lines[index] ? highlight.lines[index].map((token,part) => <span key={part} style={{color:token.color}}>{token.content}</span>) : line}</span></span>) : highlight ? highlight.lines.map((line, index) => <span className="dl-code-line" key={index}>{line.map((token, part) => <span key={part} style={{ color: token.color, fontWeight: token.fontStyle && token.fontStyle & 2 ? 700 : undefined, fontStyle: token.fontStyle && token.fontStyle & 1 ? "italic" : undefined }}>{token.content}</span>)}{index < highlight.lines.length - 1 ? "\n" : ""}</span>) : code}</code></pre>
  </div>;
}

function textOf(children: ReactNode): string {
  return Array.isArray(children) ? children.map(textOf).join("") : typeof children === "string" || typeof children === "number" ? String(children) : "";
}

export const Markdown = memo(function Markdown({ content, onActor, className }: { content: string; onActor?: (id: string) => void; className?: string }) {
  const origin = typeof location === "undefined" ? "http://localhost" : location.href;
  return <div className={["dl-markdown", className].filter(Boolean).join(" ")}><ReactMarkdown remarkPlugins={[remarkGfm]} skipHtml components={{
    a({ href, children }) {
      const actor = href && actorIdFromUrl(href, origin);
      if (actor && onActor) return <button type="button" className="dl-actor-inline" onClick={() => onActor(actor)}><MessageCircleIcon aria-hidden="true" />{children}</button>;
      const safe = href && safeLink(href);
      return safe ? <a href={safe} target="_blank" rel="noopener noreferrer">{children}</a> : <span>{children}</span>;
    },
    img({ src, alt }) {
      // No arbitrary remote image requests or local filesystem paths from text.
      return typeof src === "string" && resourceActorId(src, origin) ? <a href={src} target="_blank" rel="noreferrer"><img src={resourceSource(src)} alt={alt || t("markdown.image")} loading="lazy" /></a> : <span className="dl-image-fallback">{alt || t("markdown.image")}</span>;
    },
    pre({ children }) { return <>{children}</>; },
    code({ className: codeClass, children, node }) {
      const value = textOf(children);
      const language = /language-([^\s]+)/.exec(codeClass ?? "")?.[1] ?? "";
      const multiline = language || value.includes("\n") || node?.position?.start.line !== node?.position?.end.line;
      return multiline ? <CodeBlock code={value.replace(/\n$/, "")} language={language} /> : <code>{children}</code>;
    },
    table({ children }) { return <div className="dl-markdown-table" tabIndex={0} role="region" aria-label={t("markdown.table")}><table>{children}</table></div>; },
  }}>{content}</ReactMarkdown></div>;
});
