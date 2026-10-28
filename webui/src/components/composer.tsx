import { resourceSource } from "@/lib/transport";
import { t } from "@/lib/i18n";
import { useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent } from "react";
import { isAndroid } from "@/lib/mobile";
import { ArrowUpIcon, ChevronDownIcon, FileIcon, FolderIcon, PaperclipIcon, PlusIcon, XIcon } from "lucide-react";
import { InputGroup, InputGroupAddon, InputGroupButton, InputGroupTextarea } from "@/components/ui/input-group";
import { DropdownMenu, DropdownMenuContent, DropdownMenuGroup, DropdownMenuItem, DropdownMenuLabel, DropdownMenuRadioGroup, DropdownMenuRadioItem, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import { Spinner } from "@/components/ui/spinner";
import type { Attachment } from "@/lib/actor-api";
import { cn } from "@/lib/utils";

export type ComposerModel = { name: string; label: string; effort?: string; isDefault?: boolean };
function effortLabel(value?: string): string { return value && ["none","minimal","low","medium","high","xhigh","max"].includes(value) ? t(`effort.${value}`) : value || ""; }

export function Composer({ value, onValueChange, onSubmit, attachments, onFiles, onRemoveAttachment, uploading, busy, disabled, submissionDisabled, canManage, models, modelName, onModelChange, placeholder = t("composer.message"), compact = false }: {
  value: string; onValueChange(value: string): void; onSubmit(): void; attachments: Attachment[]; onFiles(files: File[]): void;
  onRemoveAttachment(id: string): void; uploading: boolean; busy: boolean;
  disabled?: boolean; submissionDisabled?: boolean; canManage: boolean;
  models: ComposerModel[]; modelName: string; onModelChange(name: string): void; placeholder?: string; compact?: boolean;
}) {
  const text = useRef<HTMLTextAreaElement>(null);
  const file = useRef<HTMLInputElement>(null);
  const folder = useRef<HTMLInputElement>(null);
  const [dragging, setDragging] = useState(false);
  const preference = models.some(model => model.name === modelName) ? modelName : "";
  const selected = models.find(model => model.name === preference) || models.find(model => model.isDefault);
  const hasDraft = Boolean(value.trim()) || attachments.length > 0;
  const canSend = !disabled && !submissionDisabled && !busy && !uploading && hasDraft;
  useLayoutEffect(() => {
    const element = text.current;
    if (!element) return;
    const resize = () => {
      const previous = element.getBoundingClientRect().height;
      // Measure content without exposing an intermediate auto-height frame.
      element.style.transition = "none";
      element.style.height = "auto";
      const height = element.scrollHeight;
      const limit = Math.min(240, window.innerHeight / 4);
      element.style.height = `${previous}px`;
      void element.offsetHeight;
      element.style.transition = "";
      element.style.height = `${Math.min(height, limit)}px`;
      element.style.overflowY = height > limit ? "auto" : "hidden";
    };
    resize();
    let width = element.clientWidth;
    const observer = new ResizeObserver(() => { if (element.clientWidth !== width) { width = element.clientWidth; resize(); } });
    observer.observe(element);
    window.addEventListener("resize", resize);
    return () => { observer.disconnect(); window.removeEventListener("resize", resize); };
  }, [value]);
  useEffect(() => { folder.current?.setAttribute("webkitdirectory", ""); }, []);
  function keyDown(event: KeyboardEvent<HTMLTextAreaElement>) {
    if (event.key === "Enter" && !event.shiftKey && !event.nativeEvent.isComposing && event.keyCode !== 229) { event.preventDefault(); if (canSend) onSubmit(); }
  }
  return <div className={cn("dl-composer-wrap", compact && "dl-composer-compact", "dl-primary-composer")}>
    <InputGroup className={cn("dl-composer", dragging && "dl-composer-dragging")} onDragOver={event => { if (!disabled && !uploading && event.dataTransfer.types.includes("Files")) { event.preventDefault(); setDragging(true); } }} onDragLeave={() => setDragging(false)} onDrop={event => { event.preventDefault(); setDragging(false); if (!disabled && !uploading) onFiles(Array.from(event.dataTransfer.files)); }}>
      {(attachments.length > 0 || uploading) && <InputGroupAddon align="block-start" className="dl-composer-attachments">
        {attachments.map(item => <div className="dl-attachment" key={item.id}>
          {item.media_type.startsWith("image/") ? <img src={resourceSource(item.uri)} alt={item.name} /> : <div className="dl-attachment-file" title={item.name}><FileIcon aria-hidden="true" /><span>{item.name}</span></div>}
          <InputGroupButton size="icon-xs" aria-label={t("composer.remove_value0", {value0: item.name})} className="dl-attachment-remove" onClick={() => onRemoveAttachment(item.id)}><XIcon /></InputGroupButton>
        </div>)}
        {uploading && <span className="dl-upload-indicator"><Spinner />{t("composer.uploading")}</span>}
      </InputGroupAddon>}
      <InputGroupTextarea ref={text} aria-label={t("composer.message_2")} placeholder={placeholder} value={value} disabled={disabled} onChange={event => onValueChange(event.target.value)} onKeyDown={keyDown} onPaste={event => { const files = Array.from(event.clipboardData.files); if (files.length && !disabled) { event.preventDefault(); if (!uploading) onFiles(files); } }} className="dl-composer-textarea" rows={1} />
      <InputGroupAddon align="block-end" className="dl-composer-toolbar">
        <DropdownMenu><DropdownMenuTrigger asChild><InputGroupButton size="icon-sm" className="dl-attachment-trigger" aria-label={t("composer.add_attachment")} disabled={disabled || uploading}><PlusIcon /></InputGroupButton></DropdownMenuTrigger>
          <DropdownMenuContent align="start" side="top" className="dl-composer-add-menu"><DropdownMenuLabel>{t("composer.add")}</DropdownMenuLabel><DropdownMenuGroup>
            <DropdownMenuItem onSelect={() => file.current?.click()}><PaperclipIcon data-icon="inline-start" />{t("composer.files_and_images")}</DropdownMenuItem>
            {!isAndroid() && <DropdownMenuItem onSelect={() => folder.current?.click()}><FolderIcon data-icon="inline-start" />{t("composer.folder")}<span className="dl-menu-hint">{t("composer.attach_folder")}</span></DropdownMenuItem>}
          </DropdownMenuGroup></DropdownMenuContent>
        </DropdownMenu>
        {canManage && <DropdownMenu><DropdownMenuTrigger asChild><InputGroupButton disabled={disabled || busy} aria-label={`${t("composer.models")}: ${selected?.label || t("composer.default_model")}`} className="dl-model-trigger"><span className="dl-model-name">{selected?.label || t("composer.default_model")}</span>{selected?.effort && <span className="dl-model-effort">{effortLabel(selected.effort)}</span>}<ChevronDownIcon data-icon="inline-end" /></InputGroupButton></DropdownMenuTrigger>
          <DropdownMenuContent align="end" side="top" className="dl-model-menu"><DropdownMenuLabel>{t("composer.models")}</DropdownMenuLabel><DropdownMenuRadioGroup value={preference} onValueChange={onModelChange}>
            <DropdownMenuRadioItem value="">{t("composer.use_default_model")}</DropdownMenuRadioItem>
            {models.map(model => <DropdownMenuRadioItem value={model.name} key={model.name}><span className="dl-menu-choice"><span>{model.label}</span>{(model.effort || model.name !== model.label) && <small>{effortLabel(model.effort) || model.name}</small>}</span></DropdownMenuRadioItem>)}
          </DropdownMenuRadioGroup></DropdownMenuContent>
        </DropdownMenu>}
        <InputGroupButton size="icon-sm" variant="default" className="dl-send-button" aria-label={t("composer.send_message")} disabled={!canSend} onClick={onSubmit}>{busy ? <Spinner /> : <ArrowUpIcon />}</InputGroupButton>
      </InputGroupAddon>
    </InputGroup>
    <input ref={file} className="sr-only" aria-hidden="true" tabIndex={-1} type="file" multiple onChange={event => { onFiles(Array.from(event.target.files ?? [])); event.target.value = ""; }} />
    <input ref={folder} className="sr-only" aria-hidden="true" tabIndex={-1} type="file" multiple onChange={event => { onFiles(Array.from(event.target.files ?? [])); event.target.value = ""; }} />
  </div>;
}
