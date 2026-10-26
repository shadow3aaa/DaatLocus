import { lazy, Suspense, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { RotateCwIcon } from "lucide-react";
import { formatPromptTokens, layerColor, promptContent, promptFiles, type PromptRole, type PromptFiles } from "@/lib/system-prompt";
import { t } from "@/lib/i18n";
import { Button } from "@/components/ui/button";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";
import { Spinner } from "@/components/ui/spinner";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Markdown } from "@/components/markdown";

const PromptScene = lazy(() => import("./prompt-scene"));

export function ContextSettings() {
  useTranslation();
  const [role, setRole] = useState<PromptRole>("core");
  const [reload, setReload] = useState(0);
  return <div className="dl-context-settings">
    <div className="dl-prompt-toolbar">
      <ToggleGroup type="single" value={role} className="dl-usage-range" aria-label={t("context_composition.role")} onValueChange={value => { if (value) setRole(value as PromptRole); }}>
        <ToggleGroupItem value="core">core</ToggleGroupItem>
        <ToggleGroupItem value="others">others</ToggleGroupItem>
      </ToggleGroup>
      <Button variant="ghost" size="icon-sm" aria-label={t("context_composition.refresh")} onClick={() => setReload(value => value + 1)}><RotateCwIcon /></Button>
    </div>
    <PromptView key={`${role}:${reload}`} role={role} />
  </div>;
}

function PromptView({ role }: { role: PromptRole }) {
  const connection = useRef<SVGPathElement>(null), target = useRef<HTMLSpanElement>(null);
  const [data, setData] = useState<PromptFiles | null>(null);
  const [selected, setSelected] = useState(0);
  const [content, setContent] = useState<{ index: number; text: string } | null>(null);
  const [loading, setLoading] = useState(false);
  const [failed, setFailed] = useState(false);
  const [noWebGL, setNoWebGL] = useState(false);
  useEffect(() => {
    const controller = new AbortController();
    void (async () => {
      let result = await promptFiles(role, undefined, { signal: controller.signal });
      while (result.next_cursor !== null) {
        const page = await promptFiles(role, result.next_cursor, { signal: controller.signal });
        if (page.revision !== result.revision) throw new Error("changed");
        result = { ...page, items: [...result.items, ...page.items] };
      }
      if (!controller.signal.aborted) setData(result);
    })().catch(() => { if (!controller.signal.aborted) setFailed(true); });
    return () => controller.abort();
  }, [role]);
  useEffect(() => {
    if (!data?.items[selected]) return;
    const controller = new AbortController();
    setLoading(true); setFailed(false); setContent(null);
    void (async () => {
      let offset: number | null = 0, text = "";
      while (offset !== null) {
        const page = await promptContent(role, data.items[selected].index, data.revision, offset, { signal: controller.signal });
        text += page.content; offset = page.next_offset;
        if (!controller.signal.aborted) setContent({ index: selected, text });
      }
    })().catch(() => { if (!controller.signal.aborted) setFailed(true); })
      .finally(() => { if (!controller.signal.aborted) setLoading(false); });
    return () => controller.abort();
  }, [role, data, selected]);
  const file = data?.items[selected];
  if (!data) return <div className="dl-settings-loading" role="status">{failed ? t("context_composition.failed") : <><Spinner />{t("context_composition.loading")}</>}</div>;
  return <>
    <div className="dl-prompt-studio">
      <section className="dl-prompt-scene" aria-label={t("context_composition.diagram")}>
        <div className="dl-prompt-total">{t("context_composition.total")} <strong>{formatPromptTokens(data.total_tokens)}</strong></div>
        {!noWebGL && data.items.length > 0 && <Suspense fallback={<div className="dl-settings-loading"><Spinner /></div>}><PromptScene files={data.items} selected={selected} onSelect={setSelected} onUnavailable={() => setNoWebGL(true)} connection={connection} target={target} /></Suspense>}
        <div className="dl-prompt-layer-buttons" data-fallback={noWebGL} aria-label={t("context_composition.files")}>
          {data.items.map((item, index) => <Button key={item.index} variant="ghost" aria-pressed={selected === index} onClick={() => setSelected(index)}><span className="dl-prompt-swatch" style={{ background: layerColor(item.tokens, data.items) }} />{item.name.split(/[\\/]/).at(-1)}</Button>)}
        </div>
      </section>
      <div className="dl-prompt-panel">
      {!noWebGL && <span ref={target} className="dl-prompt-target" aria-hidden="true" />}
      <aside key={selected} className="dl-prompt-content" aria-busy={loading} tabIndex={0} aria-label={file?.name ?? t("context_composition.title")}>
        {file && <><h3><span className="dl-prompt-swatch" style={{ background: layerColor(file.tokens, data.items) }} /><span title={file.name}>{file.name.split(/[\\/]/).at(-1)}</span><span className="dl-prompt-count">{formatPromptTokens(file.tokens)}</span></h3>
          {/[\\/]/.test(file.name) && <div className="dl-prompt-source">{file.name}</div>}</>}
        {failed && <Alert variant="destructive"><AlertDescription>{t("context_composition.failed")}</AlertDescription></Alert>}
        {content?.index === selected ? <Markdown key={selected} className="dl-prompt-text" content={content.text} /> : loading ? <Spinner /> : null}
        {!file && <p role="status">{t("context_composition.empty")}</p>}
      </aside>
      </div>
      {!noWebGL && <svg className="dl-prompt-connector" aria-hidden="true"><path ref={connection} /></svg>}
    </div>
  </>;
}
