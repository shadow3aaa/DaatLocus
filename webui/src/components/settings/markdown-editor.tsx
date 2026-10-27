import { useEffect, useId, useRef } from "react";
import { AtomicCodeMirrorEditor, type AtomicCodeMirrorEditorHandle } from "@atomic-editor/editor";
import "@atomic-editor/editor/styles.css";
import { Redo2Icon, SearchIcon, Undo2Icon } from "lucide-react";
import { Button } from "@/components/ui/button";
import { safeLink } from "@/lib/conversation-links";
import { t } from "@/lib/i18n";

/** Only the product adapter: Atomic owns all Markdown editing and live preview. */
export default function MarkdownEditor({ value, onChange, disabled = false }: { value: string; onChange(value: string): void; disabled?: boolean }) {
  const documentId = useId(), editor = useRef<AtomicCodeMirrorEditorHandle | null>(null);
  const label = t("context_composition.instructions");
  useEffect(() => { editor.current?.getContentDOM()?.setAttribute("aria-label", label); }, [label]);
  return <div className="dl-md-editor" inert={disabled || undefined}>
    <div className="dl-md-toolbar" role="toolbar" aria-label={t("context_composition.editor_tools")}>
      <Button size="icon-sm" variant="ghost" aria-label={t("context_composition.undo")} title={t("context_composition.undo")} onMouseDown={event => event.preventDefault()} onClick={() => editor.current?.undo()}><Undo2Icon /></Button>
      <Button size="icon-sm" variant="ghost" aria-label={t("context_composition.redo")} title={t("context_composition.redo")} onMouseDown={event => event.preventDefault()} onClick={() => editor.current?.redo()}><Redo2Icon /></Button>
      <Button size="icon-sm" variant="ghost" aria-label={t("context_composition.find")} title={t("context_composition.find")} onClick={() => editor.current?.openSearch()}><SearchIcon /></Button>
    </div>
    <div className="dl-md-surface"><AtomicCodeMirrorEditor documentId={documentId} markdownSource={value} onMarkdownChange={onChange} editorHandleRef={editor} readOnly={disabled} onLinkClick={url => { const safe = safeLink(url); if (safe) window.open(safe, "_blank", "noopener,noreferrer"); }} /></div>
  </div>;
}
