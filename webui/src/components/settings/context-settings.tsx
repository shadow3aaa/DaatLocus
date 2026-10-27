import { lazy, Suspense, useDeferredValue, useEffect, useId, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { CheckIcon, PencilIcon, PlusIcon, RotateCwIcon, SearchIcon, Trash2Icon } from "lucide-react";
import { allInstructionLayers, allPromptFiles, displayPromptFiles, formatPromptTokens, instructionLayer, instructionLayers, layerColor, layerError, promptLayerWindow, LIBRARY_VISIBLE_LAYERS as VISIBLE_LAYERS, promptContent, promptFileKey, replacePromptLayers, saveInstructionLayer, type InstructionLayer, type InstructionLayerDetail, type PromptFile, type PromptRole, type PromptFiles } from "@/lib/system-prompt";
import { t } from "@/lib/i18n";
import { Button } from "@/components/ui/button";
import { FloatingSearchBar } from "@/components/floating-search";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";
import { Spinner } from "@/components/ui/spinner";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Markdown } from "@/components/markdown";
import { InstructionEditor } from "./instruction-editor";

const PromptScene = lazy(() => import("./prompt-scene"));

export function ContextSettings() {
  useTranslation();
  const [role, setRole] = useState<PromptRole>("core");
  return <PromptWorkspace key={role} role={role} onRoleChange={setRole} />;
}

function PromptWorkspace({ role, onRoleChange }: { role: PromptRole; onRoleChange(role: PromptRole): void }) {
  const target = useRef<HTMLSpanElement>(null), assemblyLine = useRef<SVGPathElement>(null), libraryLine = useRef<SVGPathElement>(null);
  const removal = useRef<SVGPathElement>(null), removeTarget = useRef<HTMLButtonElement>(null);
  const creation = useRef<SVGPathElement>(null), createTarget = useRef<HTMLButtonElement>(null);
  const detailPanel = useRef<HTMLDivElement>(null);
  const [data, setData] = useState<PromptFiles | null>(null), [reload, setReload] = useState(0);
  const [inserted, setInserted] = useState<string>(), [assemblyKey, setAssemblyKey] = useState<string>();
  const files = useMemo(() => displayPromptFiles(data?.items ?? [], inserted), [data, inserted]);
  const assemblyIndex = Math.max(0, files.findIndex(file => promptFileKey(file) === assemblyKey));
  const assemblyFiles = useMemo(() => promptLayerWindow(files, assemblyIndex, files.length), [files, assemblyIndex]);
  const [library, setLibrary] = useState<InstructionLayer[]>([]), [next, setNext] = useState<number | null>(null);
  const [query, setQuery] = useState(""), search = useDeferredValue(query);
  const [searchOpen, setSearchOpen] = useState(false), libraryId = useId();
  const [start, setStart] = useState(0), [libraryKey, setLibraryKey] = useState<string>();
  const [libraryLoading, setLibraryLoading] = useState(false);
  const [libraryReload, setLibraryReload] = useState(0);
  const offset = library.length ? ((start % library.length) + library.length) % library.length : 0;
  const libraryFiles = useMemo<PromptFile[]>(() => promptLayerWindow(library, offset).map(layer => ({ ...layer, layer_id: layer.id, index: -1 })), [library, offset]);
  const libraryIndex = libraryFiles.findIndex(file => file.layer_id === libraryKey);
  const [source, setSource] = useState<"assembly" | "library">("assembly");
  const file = source === "assembly" ? files[assemblyIndex] : library.find(layer => layer.id === libraryKey);
  const fileKey = file && ("id" in file ? file.id : promptFileKey(file));
  const [content, setContent] = useState<{ key: string; text: string } | null>(null);
  const [detail, setDetail] = useState<InstructionLayerDetail>();
  const [loading, setLoading] = useState(false), [busy, setBusy] = useState(false), [error, setError] = useState("");
  const [noWebGL, setNoWebGL] = useState(false);
  const [editing, setEditing] = useState(false), [editingFile, setEditingFile] = useState<InstructionLayerDetail | null>(null);
  const [draft, setDraft] = useState({ name: "", content: "" });
  const pendingInsert = useRef(false);
  const revealLibrary = useRef<string | undefined>(undefined);
  useEffect(() => {
    const controller = new AbortController(); setError("");
    void allPromptFiles(role, { signal: controller.signal }).then(setData).catch(error => { if (!controller.signal.aborted) setError(layerError(error)); });
    return () => controller.abort();
  }, [role, reload]);
  useEffect(() => {
    if (!data?.editable_layers) return;
    const controller = new AbortController(); setLibraryLoading(true);
    const request = revealLibrary.current ? allInstructionLayers(search, { signal: controller.signal }) : instructionLayers(search, 0, { signal: controller.signal });
    void request.then(page => { const index = revealLibrary.current ? Math.max(0, page.items.findIndex(file => file.id === revealLibrary.current)) : 0; setLibrary(page.items); setNext(page.next_cursor); setStart(index); setLibraryKey(page.items[index]?.id); revealLibrary.current = undefined; if (search.trim()) setSource("library"); })
      .catch(error => { if (!controller.signal.aborted) setError(layerError(error)); })
      .finally(() => { if (!controller.signal.aborted) setLibraryLoading(false); });
    return () => controller.abort();
  }, [search, libraryReload, data?.editable_layers]);
  useEffect(() => {
    if (editing) return;
    if (!file || !fileKey) { setContent(null); setDetail(undefined); setLoading(false); return; }
    const controller = new AbortController(); setLoading(true); setContent(null); setDetail(undefined);
    void (async () => {
      if (source === "library") {
        const result = await instructionLayer(fileKey, { signal: controller.signal });
        if (!controller.signal.aborted) { setDetail(result); setContent({ key: fileKey, text: result.content }); }
      } else if (data && "index" in file) {
        let offset: number | null = 0, text = "";
        while (offset !== null) {
          const page = await promptContent(role, file.index, data.revision, offset, { signal: controller.signal });
          text += page.content; offset = page.next_offset;
          if (!controller.signal.aborted) setContent({ key: fileKey, text });
        }
      }
    })().catch(error => { if (!controller.signal.aborted) setError(layerError(error)); })
      .finally(() => { if (!controller.signal.aborted) setLoading(false); });
    return () => controller.abort();
  }, [source, fileKey, data?.revision, library, editing, role]);
  useEffect(() => {
    const root = detailPanel.current, term = query.trim().toLocaleLowerCase();
    if (!root || !CSS.highlights) return;
    const ranges: Range[] = [];
    if (term && source === "library" && !loading) {
      const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
      while (walker.nextNode()) {
        const node = walker.currentNode;
        if (node.parentElement?.closest("button")) continue;
        const text = node.textContent?.toLocaleLowerCase() ?? "";
        for (let index = text.indexOf(term); index >= 0; index = text.indexOf(term, index + term.length)) {
          const range = new Range(); range.setStart(node, index); range.setEnd(node, index + term.length); ranges.push(range);
        }
      }
    }
    CSS.highlights.set("dl-prompt-search", new Highlight(...ranges));
    return () => { CSS.highlights.delete("dl-prompt-search"); };
  }, [query, content, source, loading, fileKey]);
  async function refresh() { setData(await allPromptFiles(role)); }
  async function enable(id: string, enabled: boolean) {
    if (!data) return;
    const ids = data.items.flatMap(file => file.layer_id ? [file.layer_id] : []);
    await replacePromptLayers(role, data.revision, enabled ? [...ids.filter(value => value !== id), id] : ids.filter(value => value !== id));
    if (enabled) { setInserted(id); setAssemblyKey(id); }
    else setAssemblyKey(undefined);
    await refresh();
  }
  async function changeMembership(id: string, enabled: boolean) {
    setBusy(true); setError("");
    try { await enable(id, enabled); }
    catch (error) { setError(layerError(error)); }
    finally { setBusy(false); }
  }
  function create() {
    pendingInsert.current = true; setEditingFile(null); setDraft({ name: "", content: "" }); setSource("library"); setEditing(true); setError("");
  }
  async function save(value: { name: string; content: string }) {
    if (busy || !value.name.trim() || !value.content.trim() || /[/\\]/.test(value.name)) return;
    setBusy(true); setError("");
    try {
      const saved = await saveInstructionLayer(value, editingFile ?? undefined);
      setEditingFile(saved);
      if (pendingInsert.current) { await enable(saved.id, true); pendingInsert.current = false; }
      else await refresh();
      const result = await allInstructionLayers("");
      revealLibrary.current = search ? saved.id : undefined;
      setQuery(""); setLibrary(result.items); setNext(result.next_cursor);
      setLibraryKey(saved.id); setStart(Math.max(0, result.items.findIndex(file => file.id === saved.id)));
      setEditing(false);
    } catch (error) { setError(layerError(error)); }
    finally { setBusy(false); }
  }
  const pageBusy = useRef(false);
  async function step(direction: number) {
    if (editing || !library.length || pageBusy.current) return;
    const destination = (offset + direction + library.length) % library.length;
    if (direction < 0 && offset === 0 && next !== null) {
      pageBusy.current = true;
      try { const page = await allInstructionLayers(search); setLibrary(page.items); setNext(page.next_cursor); setStart(page.items.length - 1); setLibraryKey(page.items.at(-1)?.id); setSource("library"); }
      catch (error) { setError(layerError(error)); }
      finally { pageBusy.current = false; }
    } else if (direction > 0 && offset + VISIBLE_LAYERS >= library.length && next !== null) {
      pageBusy.current = true;
      try { const page = await instructionLayers(search, next); const all = [...library, ...page.items]; setLibrary(all); setNext(page.next_cursor); setStart(offset + 1); setLibraryKey(all[(offset + 1) % all.length]?.id); setSource("library"); }
      catch (error) { setError(layerError(error)); }
      finally { pageBusy.current = false; }
    } else {
      setStart(destination);
      setLibraryKey(library[destination]?.id); setSource("library");
    }
  }
  const selectAssembly = (index: number) => { if (!editing) { setAssemblyKey(promptFileKey(files[index])); setSource("assembly"); } };
  const selectLibrary = (index: number) => { if (!editing) { const key = libraryFiles[index]?.layer_id; if (!key) return; setStart(library.findIndex(file => file.id === key)); setLibraryKey(key); setSource("library"); } };
  const focusAssembly = () => { if (!editing) setSource("assembly"); };
  const focusLibrary = () => { if (!editing) { if (libraryIndex < 0) setLibraryKey(libraryFiles[0]?.layer_id); setSource("library"); } };
  function openSearch() { focusLibrary(); setSearchOpen(true); }
  function closeSearch() { setSearchOpen(false); setQuery(""); }
  useEffect(() => {
    const shortcut = (event: KeyboardEvent) => { if (!editing && (event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "f") { event.preventDefault(); event.stopPropagation(); setSource("library"); setLibraryKey(libraryFiles[0]?.layer_id); setSearchOpen(true); } };
    window.addEventListener("keydown", shortcut, true); return () => window.removeEventListener("keydown", shortcut, true);
  }, [editing, libraryFiles]);
  const lockedSource = source === "assembly", removable = lockedSource ? files[assemblyIndex]?.layer_id : undefined;
  const included = fileKey && data?.items.some(file => file.layer_id === fileKey);
  const assemblyLabel = t("context_composition.file_label", { kind: t("context_composition.assembly"), name: assemblyFiles[0]?.name ?? "" });
  const libraryLabel = t("context_composition.file_label", { kind: t("context_composition.library"), name: libraryFiles[0]?.name ?? "" });
  return <div className="dl-context-settings" data-search-open={searchOpen}>
    {/* Keep this standard selector native until the CSS optimizer supports ::highlight. */}
    <style>{"::highlight(dl-prompt-search) { background: color-mix(in srgb,var(--primary) 22%,var(--background)); color: var(--foreground); }"}</style>
    <FloatingSearchBar open={searchOpen && !editing} query={query} onQueryChange={setQuery} onClose={closeSearch} busy={libraryLoading} controls={libraryId} label={t("context_composition.search_layers")} />
    <div className="dl-prompt-toolbar"><ToggleGroup type="single" value={role} className="dl-usage-range" disabled={editing} aria-label={t("context_composition.role")} onValueChange={value => { if (value) onRoleChange(value as PromptRole); }}><ToggleGroupItem value="core">core</ToggleGroupItem><ToggleGroupItem value="others">others</ToggleGroupItem></ToggleGroup>
      <div className="dl-prompt-toolbar-actions">{data && <span className="dl-prompt-total">{t("context_composition.total")} {formatPromptTokens(data.total_tokens)}</span>}{data?.editable_layers && <Button variant="ghost" size="icon-sm" disabled={editing} aria-label={t("context_composition.search_layers")} aria-keyshortcuts="Control+f Meta+f" onClick={openSearch}><SearchIcon /></Button>}<Button variant="ghost" size="icon-sm" disabled={busy || editing} aria-label={t("context_composition.refresh")} onClick={() => { setInserted(undefined); setReload(value => value + 1); setLibraryReload(value => value + 1); }}><RotateCwIcon /></Button></div>
    </div>
    {error && !editing && <Alert variant="destructive"><AlertDescription>{error}</AlertDescription></Alert>}
    {!data ? <div className="dl-settings-loading" role="status"><Spinner /></div> : <div className="dl-context-studio" data-focus={source}>
      <div className="dl-prompt-diagrams" inert={editing || undefined}>
        <Button className="dl-prompt-focus-label" data-source="assembly" variant="ghost" aria-pressed={lockedSource} onClick={focusAssembly} title={assemblyLabel}><span className="dl-prompt-file-name">{assemblyLabel}</span></Button>
        {data.editable_layers && <Button className="dl-prompt-focus-label" data-source="library" variant="ghost" aria-pressed={!lockedSource} onClick={focusLibrary} title={libraryLabel}><span className="dl-prompt-file-name">{libraryLabel}</span></Button>}
        <section className="dl-prompt-scene" data-source="assembly" data-focused={lockedSource} aria-label={t("context_composition.assembly")}>
          {!noWebGL && <Suspense fallback={<Spinner />}><PromptScene files={assemblyFiles} selected={0} active={!editing && lockedSource} onSelect={index => selectAssembly((assemblyIndex + index) % files.length)} onStep={direction => { if (files.length) selectAssembly((assemblyIndex + direction + files.length) % files.length); }} onUnavailable={() => setNoWebGL(true)} connection={assemblyLine} target={target} removal={removal} removeTarget={removeTarget} /></Suspense>}
          {noWebGL && <select className="dl-prompt-fallback" aria-label={t("context_composition.assembly")} value={assemblyIndex} onChange={event => selectAssembly(Number(event.target.value))}>{files.map((file, index) => <option key={promptFileKey(file)} value={index}>{file.name}</option>)}</select>}
          {removable && data.editable_layers && <Button ref={removeTarget} className="dl-prompt-remove" size="icon-lg" variant="outline" disabled={busy} aria-label={t("context_composition.remove_named", { name: file?.name })} title={t("context_composition.remove")} onClick={() => void changeMembership(String(removable), false)}>{busy ? <Spinner /> : <Trash2Icon />}</Button>}
          {!lockedSource && <button type="button" className="dl-prompt-refocus" tabIndex={-1} aria-label={t("context_composition.assembly")} onClick={focusAssembly} />}
        </section>
        {data.editable_layers && <section id={libraryId} className="dl-prompt-scene dl-prompt-library-scene" data-source="library" data-focused={!lockedSource} aria-label={t("context_composition.library")}>
          {!noWebGL ? <Suspense fallback={<Spinner />}><PromptScene files={libraryFiles} selected={libraryIndex} active={!editing && !lockedSource && libraryIndex >= 0} onSelect={selectLibrary} onStep={direction => void step(direction)} onUnavailable={() => setNoWebGL(true)} connection={libraryLine} target={target} removal={removal} removeTarget={removeTarget} creation={creation} createTarget={createTarget} /></Suspense> : <select className="dl-prompt-fallback" aria-label={t("context_composition.library")} value={libraryKey ?? ""} onChange={event => { const index = library.findIndex(file => file.id === event.target.value); setStart(index); setLibraryKey(event.target.value); setSource("library"); }}><option value="" disabled>{t("context_composition.files")}</option>{library.map(layer => <option key={layer.id} value={layer.id}>{layer.name}</option>)}</select>}
          {libraryLoading && <Spinner className="dl-library-loading" />}
          {!libraryLoading && !!search.trim() && !library.length && <p className="dl-prompt-library-empty">{t("context_composition.no_layers")}</p>}
          <Button ref={createTarget} className="dl-layer-create" size="icon-lg" disabled={busy} aria-label={t("context_composition.new_layer")} title={t("context_composition.new_layer")} onClick={create}><PlusIcon /></Button>
          {lockedSource && <button type="button" className="dl-prompt-refocus" tabIndex={-1} aria-label={t("context_composition.library")} onClick={focusLibrary} />}
        </section>}
      </div>
      <div className="dl-prompt-detail-panel" ref={detailPanel}>
        <span ref={target} className="dl-prompt-target" aria-hidden="true" />
        <header className="dl-prompt-detail-heading">
          {file && <span className="dl-prompt-file-swatch" aria-hidden="true" style={{ background: layerColor(file.tokens, lockedSource ? assemblyFiles : libraryFiles) }} />}
          <h3>{file?.name ?? t("context_composition.select_layer")}</h3>{file && <span className="dl-prompt-count">{formatPromptTokens(file.tokens)}</span>}
          {!lockedSource && detail && <><Button variant="ghost" size="icon-sm" disabled={busy} aria-label={t("context_composition.edit_layer")} onClick={() => { setDraft({ name: detail.name, content: detail.content }); setEditingFile(detail); pendingInsert.current = false; setEditing(true); }}><PencilIcon /></Button><Button variant="outline" disabled={busy || !!included} onClick={() => void changeMembership(detail.id, true)}>{included ? <CheckIcon data-icon="inline-start" /> : <PlusIcon data-icon="inline-start" />}{t(included ? "context_composition.inserted" : "context_composition.insert")}</Button></>}
        </header>
        <aside className="dl-prompt-content" tabIndex={0} aria-label={file?.name ?? t("context_composition.select_layer")} aria-busy={loading}>{loading ? <Spinner /> : content?.key === fileKey && content ? <Markdown className="dl-prompt-text" content={content.text} /> : null}</aside>
      </div>
      {!noWebGL && <svg className="dl-prompt-connector" aria-hidden="true">{lockedSource ? <path ref={assemblyLine} data-connection="content" /> : libraryIndex >= 0 && <path ref={libraryLine} data-connection="content" />}{removable && <path ref={removal} data-connection="remove" />}{data.editable_layers && <path ref={creation} data-connection="add" />}</svg>}
    </div>}
    {editing && <InstructionEditor initial={draft} busy={busy} error={error} onSave={value => void save(value)} onClose={() => { setEditing(false); setError(""); }} />}
  </div>;
}
