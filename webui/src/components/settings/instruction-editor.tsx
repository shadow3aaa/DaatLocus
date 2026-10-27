import { lazy, Suspense, useEffect, useState, useSyncExternalStore } from "react";
import { ArrowLeftIcon, XIcon } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Spinner } from "@/components/ui/spinner";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { t } from "@/lib/i18n";

const MarkdownEditor = lazy(() => import("./markdown-editor"));
const subscribe = (listener: () => void) => { const media = matchMedia("(max-width:767px)"); media.addEventListener("change", listener); return () => media.removeEventListener("change", listener); };

export function InstructionEditor({ initial, busy, error, onSave, onClose }: {
  initial: { name: string; content: string }; busy: boolean; error: string;
  onSave(draft: { name: string; content: string }): void; onClose(): void;
}) {
  const narrow = useSyncExternalStore(subscribe, () => matchMedia("(max-width:767px)").matches, () => false);
  const [draft, setDraft] = useState(initial);
  const valid = draft.name.trim() && draft.content.trim() && !/[/\\]/.test(draft.name);
  useEffect(() => {
    const close = (event: KeyboardEvent) => { if (event.key === "Escape" && !busy) { event.preventDefault(); onClose(); } };
    if (narrow) window.addEventListener("keydown", close);
    return () => window.removeEventListener("keydown", close);
  }, [narrow, busy, onClose]);
  const title = t(initial.name ? "context_composition.edit_layer" : "context_composition.new_layer");
  const body = <>
    <header className="dl-instruction-editor-heading">
      <Button autoFocus={narrow} variant="ghost" size="icon" disabled={busy} aria-label={t("context_composition.cancel")} onClick={onClose}>{narrow ? <ArrowLeftIcon /> : <XIcon />}</Button>
      {narrow ? <h2>{title}</h2> : <DialogTitle>{title}</DialogTitle>}
      <Button disabled={busy || !valid} onClick={() => onSave(draft)}>{busy && <Spinner />}{t("context_composition.save")}</Button>
    </header>
    {error && <Alert variant="destructive"><AlertDescription>{error}</AlertDescription></Alert>}
    <Input aria-label={t("context_composition.file_name")} value={draft.name} placeholder={t("context_composition.name_placeholder")} disabled={busy} onChange={event => setDraft(draft => ({ ...draft, name: event.target.value }))} />
    <Suspense fallback={<div className="dl-settings-loading"><Spinner /></div>}><MarkdownEditor value={draft.content} disabled={busy} onChange={content => setDraft(draft => ({ ...draft, content }))} /></Suspense>
  </>;
  const shortcuts = (event: React.KeyboardEvent) => { if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "s") { event.preventDefault(); if (valid && !busy) onSave(draft); } };
  return narrow ? <section className="dl-instruction-editor-page" aria-label={title} onKeyDownCapture={shortcuts}>{body}</section> : <Dialog open onOpenChange={open => { if (!open && !busy) onClose(); }}><DialogContent className="dl-instruction-editor-dialog" showCloseButton={false} aria-describedby={undefined} onKeyDownCapture={shortcuts}>{body}</DialogContent></Dialog>;
}
