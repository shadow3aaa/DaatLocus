import { resourceSource } from "@/lib/transport";
import { t } from "@/lib/i18n";
import { useEffect, useRef, useState, type KeyboardEvent } from "react";
import { isAndroid } from "@/lib/mobile";
import { ArrowUpIcon, ChevronDownIcon, FileIcon, FolderIcon, HandIcon, PaperclipIcon, PlusIcon, ShieldCheckIcon, ShieldIcon, SquareIcon, XIcon } from "lucide-react";
import { InputGroup, InputGroupAddon, InputGroupButton, InputGroupTextarea } from "@/components/ui/input-group";
import { DropdownMenu, DropdownMenuContent, DropdownMenuGroup, DropdownMenuItem, DropdownMenuLabel, DropdownMenuRadioGroup, DropdownMenuRadioItem, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import { Spinner } from "@/components/ui/spinner";
import type { ApprovalMode, Attachment } from "@/lib/actor-api";
import { cn } from "@/lib/utils";

export type ComposerModel = { name: string; label: string; effort?: string; isDefault?: boolean };
export function modeLabel(mode: ApprovalMode): string {
  return t({auto:"composer.auto_approval", ask:"composer.ask_for_approval", none:"composer.full_access"}[mode]);
}
function effortLabel(value?: string): string { return value && ["none","minimal","low","medium","high","xhigh","max"].includes(value) ? t(`effort.${value}`) : value || ""; }
const ModeIcons = { auto: ShieldCheckIcon, ask: HandIcon, none: ShieldIcon };

export function Composer({ value, onValueChange, onSubmit, attachments, onFiles, onRemoveAttachment, uploading, busy, working, onStop, disabled, submissionDisabled, canManage, mode, onModeChange, models, modelName, onModelChange, placeholder = t("composer.message"), compact = false, presentation = "work" }: {
  value: string; onValueChange(value: string): void; onSubmit(): void; attachments: Attachment[]; onFiles(files: File[]): void;
  onRemoveAttachment(id: string): void; uploading: boolean; busy: boolean; working: boolean; onStop(): void;
  disabled?: boolean; submissionDisabled?: boolean; canManage: boolean; mode: ApprovalMode; onModeChange(mode: ApprovalMode): void;
  models: ComposerModel[]; modelName: string; onModelChange(name: string): void; placeholder?: string; compact?: boolean; presentation?: "primary" | "work";
}) {
  const text = useRef<HTMLTextAreaElement>(null);
  const file = useRef<HTMLInputElement>(null);
  const folder = useRef<HTMLInputElement>(null);
  const [dragging, setDragging] = useState(false);
  const ModeIcon = ModeIcons[mode];
  const preference = models.some(model => model.name === modelName) ? modelName : "";
  const selected = models.find(model => model.name === preference) || models.find(model => model.isDefault);
  const canSend = !disabled && !submissionDisabled && !busy && !uploading && (Boolean(value.trim()) || attachments.length > 0);
  useEffect(() => {
    const element = text.current;
    if (!element) return;
    const resize = () => { element.style.height = "auto"; const height = element.scrollHeight; const limit = Math.min(240, window.innerHeight / 4); element.style.height = `${Math.min(height, limit)}px`; element.style.overflowY = height > limit ? "auto" : "hidden"; };
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
  return <div className={cn("dl-composer-wrap", compact && "dl-composer-compact", presentation === "primary" && "dl-primary-composer")}>
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
        <DropdownMenu><DropdownMenuTrigger asChild><InputGroupButton size="icon-sm" aria-label={t("composer.add_attachment")} disabled={disabled || uploading}><PlusIcon /></InputGroupButton></DropdownMenuTrigger>
          <DropdownMenuContent align="start" side="top" className="dl-composer-add-menu"><DropdownMenuLabel>{t("composer.add")}</DropdownMenuLabel><DropdownMenuGroup>
            <DropdownMenuItem onSelect={() => file.current?.click()}><PaperclipIcon data-icon="inline-start" />{t("composer.files_and_images")}</DropdownMenuItem>
            {!isAndroid() && <DropdownMenuItem onSelect={() => folder.current?.click()}><FolderIcon data-icon="inline-start" />{t("composer.folder")}<span className="dl-menu-hint">{t("composer.attach_folder")}</span></DropdownMenuItem>}
          </DropdownMenuGroup></DropdownMenuContent>
        </DropdownMenu>
        {canManage && presentation === "work" && <DropdownMenu><DropdownMenuTrigger asChild><InputGroupButton disabled={disabled || busy || working} className="dl-mode-trigger" aria-label={modeLabel(mode)} title={modeLabel(mode)}><ModeIcon data-icon="inline-start" /><span className="dl-mode-label">{modeLabel(mode)}</span></InputGroupButton></DropdownMenuTrigger>
          <DropdownMenuContent align="start" side="top" className="dl-mode-menu"><DropdownMenuLabel>{t("composer.action_approval")}</DropdownMenuLabel><DropdownMenuRadioGroup value={mode} onValueChange={next => onModeChange(next as ApprovalMode)}>
            {(["ask", "auto", "none"] as const).map(key => <DropdownMenuRadioItem value={key} key={key}>{modeLabel(key)}</DropdownMenuRadioItem>)}
          </DropdownMenuRadioGroup></DropdownMenuContent></DropdownMenu>}
        <div className="dl-composer-toolbar-spacer" />
        {canManage && presentation === "work" && <DropdownMenu><DropdownMenuTrigger asChild><InputGroupButton disabled={disabled || busy || working} className="dl-model-trigger"><span className="dl-model-name">{selected?.label || t("composer.default_model")}</span>{selected?.effort && <span className="dl-model-effort">{effortLabel(selected.effort)}</span>}<ChevronDownIcon data-icon="inline-end" /></InputGroupButton></DropdownMenuTrigger>
          <DropdownMenuContent align="end" side="top" className="dl-model-menu"><DropdownMenuLabel>{t("composer.models")}</DropdownMenuLabel><DropdownMenuRadioGroup value={preference} onValueChange={onModelChange}>
            <DropdownMenuRadioItem value="">{t("composer.use_default_model")}</DropdownMenuRadioItem>
            {models.map(model => <DropdownMenuRadioItem value={model.name} key={model.name}><span className="dl-menu-choice"><span>{model.label}</span>{(model.effort || model.name !== model.label) && <small>{effortLabel(model.effort) || model.name}</small>}</span></DropdownMenuRadioItem>)}
          </DropdownMenuRadioGroup></DropdownMenuContent>
        </DropdownMenu>}
        {working && presentation === "work" && <InputGroupButton size="icon-sm" variant="default" className="dl-send-button dl-stop-button" aria-label={t("composer.stop_task")} disabled={busy} onClick={onStop}><SquareIcon /></InputGroupButton>}{(presentation === "primary" || !working || canSend) && <InputGroupButton size="icon-sm" variant="default" className="dl-send-button" aria-label={working && presentation !== "primary" ? t("composer.queue_message") : t("composer.send_message")} disabled={!canSend} onClick={onSubmit}>{busy ? <Spinner /> : <ArrowUpIcon />}</InputGroupButton>}
      </InputGroupAddon>
    </InputGroup>
    <input ref={file} className="sr-only" aria-hidden="true" tabIndex={-1} type="file" multiple onChange={event => { onFiles(Array.from(event.target.files ?? [])); event.target.value = ""; }} />
    <input ref={folder} className="sr-only" aria-hidden="true" tabIndex={-1} type="file" multiple onChange={event => { onFiles(Array.from(event.target.files ?? [])); event.target.value = ""; }} />
  </div>;
}
