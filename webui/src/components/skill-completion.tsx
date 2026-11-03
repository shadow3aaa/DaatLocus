import { useEffect, useId, useRef, useState, type KeyboardEvent, type RefObject } from "react";
import { BoxIcon, XIcon } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { PopoverContent } from "@/components/ui/popover";
import { Spinner } from "@/components/ui/spinner";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { getDevices, getStage } from "@/lib/stage-api";
import { getSkills, mentionsSkill, skillKey, skillQuery, type SkillReference, type SkillsPage } from "@/lib/skills";
import { t } from "@/lib/i18n";

type Source = { query?: string; stage_id: string; label: string; page?: SkillsPage; busy?: boolean; failed?: boolean };
export function useSkillCompletion(text: RefObject<HTMLTextAreaElement | null>, value: string, onChange: (value: string) => void, skills: SkillReference[], onSkillsChange: (skills: SkillReference[]) => void, disabled?: boolean) {
  const id = useId();
  const [caret, setCaret] = useState(0);
  const [dismissed, setDismissed] = useState("");
  const [composing, setComposing] = useState(false);
  const [devices, setDevices] = useState<Source[]>([]);
  const [sources, setSources] = useState<Source[]>([]);
  const [failed, setFailed] = useState(false);
  const [offline, setOffline] = useState(0);
  const [retry, setRetry] = useState(0);
  const [activeKey, setActiveKey] = useState("");
  const requests = useRef<AbortController | null>(null);
  const token = !disabled && !composing && dismissed !== `${value}:${caret}` ? skillQuery(value, caret) : null;
  const open = Boolean(token);
  const query = token?.query;
  const rows = sources.filter(source => source.query === query).flatMap(source => (source.page?.skills || []).map(skill => ({ ...skill, stage_id: source.stage_id, workspace: source.page!.workspace, device: source.label })));
  const active = Math.max(0, rows.findIndex(skill => skillKey(skill) === activeKey));
  const close = () => setDismissed(`${value}:${caret}`);
  useEffect(() => {
    if (!open) return;
    const abort = new AbortController();
    setFailed(false); setDevices([]); setOffline(0);
    void (async () => {
      const local = await getStage({signal: abort.signal});
      const devices: Source[] = [{stage_id: local.stage_id, label: local.settings.label}];
      let after: string | undefined;
      let offline = 0;
      do {
        const page = await getDevices({after, signal: abort.signal});
        for (const device of page.items) {
          if (device.stage_id === local.stage_id) continue;
          if (device.connection.status === "online") devices.push({stage_id: device.stage_id, label: device.label || device.stage_id});
          else offline++;
        }
        after = page.next_cursor || undefined;
      } while (after);
      if (!abort.signal.aborted) { setDevices(devices); setOffline(offline); }
    })().catch(() => { if (!abort.signal.aborted) setFailed(true); });
    return () => abort.abort();
  }, [open, retry]);
  useEffect(() => {
    const abort = new AbortController();
    requests.current = abort;
    setSources([]); setActiveKey("");
    if (query === undefined || !devices.length) return () => abort.abort();
    setSources(devices.map(source => ({...source, query, busy: true})));
    const timer = setTimeout(() => {
      for (const source of devices) void getSkills(source.stage_id, query, null, {signal: abort.signal})
        .then(page => { if (!abort.signal.aborted) setSources(current => current.map(item => item.stage_id === source.stage_id ? {...source, query, page} : item)); })
        .catch(() => { if (!abort.signal.aborted) setSources(current => current.map(item => item.stage_id === source.stage_id ? {...source, query, failed: true} : item)); });
    }, 180);
    return () => { clearTimeout(timer); abort.abort(); };
  }, [query, devices]);
  useEffect(() => { if (open) document.getElementById(`${id}-${active}`)?.scrollIntoView({block: "nearest"}); }, [open, id, active, rows.length]);
  function choose(index: number) {
    const skill = rows[index];
    if (!token || !skill) return;
    const insertion = `$${skill.name} `;
    const next = value.slice(0, token.start) + insertion + value.slice(token.end);
    onChange(next);
    const {stage_id, workspace, path, name} = skill;
    const retained = skills.filter(item => mentionsSkill(next, item.name));
    onSkillsChange(retained.some(item => skillKey(item) === skillKey(skill)) ? retained : [...retained, {stage_id, workspace, path, name}]);
    const cursor = token.start + insertion.length;
    setCaret(cursor);
    requestAnimationFrame(() => { text.current?.focus(); text.current?.setSelectionRange(cursor, cursor); });
  }
  function keyDown(event: KeyboardEvent<HTMLTextAreaElement>): boolean {
    if (!open || event.nativeEvent.isComposing || event.keyCode === 229) return false;
    if (event.key === "Escape") { event.preventDefault(); event.stopPropagation(); close(); return true; }
    if (["ArrowDown", "ArrowUp"].includes(event.key)) {
      event.preventDefault();
      if (rows.length) setActiveKey(skillKey(rows[(active + (event.key === "ArrowDown" ? 1 : -1) + rows.length) % rows.length]));
      return true;
    }
    if ((event.key === "Enter" && !event.shiftKey) || (event.key === "Tab" && rows.length)) { event.preventDefault(); choose(active); return true; }
    return false;
  }
  async function more(source: Source) {
    const abort = requests.current;
    if (query === undefined || source.page?.next_cursor == null || source.busy || !abort) return;
    setSources(current => current.map(item => item === source ? {...item, busy: true} : item));
    try {
      const page = await getSkills(source.stage_id, query, source.page.next_cursor, {signal: abort.signal});
      if (!abort.signal.aborted) setSources(current => current.map(item => item.stage_id === source.stage_id ? {...source, page: {...page, skills: [...source.page!.skills, ...page.skills]}} : item));
    } catch { if (!abort.signal.aborted) setSources(current => current.map(item => item.stage_id === source.stage_id ? {...item, busy: false, failed: true} : item)); }
  }
  return {id, open, close, rows, active, sources, failed, offline, busy: !devices.length && !failed || sources.some(source => source.busy), choose, more, keyDown, retry: () => setRetry(value => value + 1), hover: (index: number) => setActiveKey(skillKey(rows[index])), track: (element: HTMLTextAreaElement) => { setCaret(element.selectionEnd); if (element.selectionStart !== element.selectionEnd) setDismissed(`${element.value}:${element.selectionEnd}`); }, composition: setComposing};
}

export function SkillSuggestions({completion: c}: {completion: ReturnType<typeof useSkillCompletion>}) {
  return <PopoverContent side="top" align="start" sideOffset={10} collisionPadding={12} className="dl-skill-suggestions" role="listbox" id={c.id} aria-label={t("composer.skills")} onOpenAutoFocus={event => event.preventDefault()} onCloseAutoFocus={event => event.preventDefault()} onEscapeKeyDown={event => { event.preventDefault(); event.stopPropagation(); c.close(); }}>
    {c.rows.map((skill, index) => <Tooltip key={skillKey(skill)}><TooltipTrigger asChild><Button variant="ghost" role="option" aria-selected={index === c.active} id={`${c.id}-${index}`} tabIndex={-1} className="dl-skill-option" onMouseDown={event => event.preventDefault()} onMouseEnter={() => c.hover(index)} onClick={() => c.choose(index)}><BoxIcon data-icon="inline-start" /><span className="dl-skill-name">{skill.name}</span><span className="dl-skill-description">{skill.description}</span><span className="dl-skill-source">{skill.device} · {t(`composer.skill_scope_${skill.scope}`)}</span></Button></TooltipTrigger><TooltipContent><span className="dl-skill-path">{skill.path}</span></TooltipContent></Tooltip>)}
    {c.busy && <div className="dl-skill-notice" role="status"><Spinner />{t("composer.skills_loading")}</div>}
    {!c.busy && !c.rows.length && !c.failed && !c.sources.some(source => source.failed) && <div className="dl-skill-notice">{t("composer.skills_empty")}</div>}
    {(c.failed || c.sources.some(source => source.failed)) && <Button variant="ghost" onMouseDown={event => event.preventDefault()} onClick={c.retry}>{t("composer.skills_failed")} · {t("app.retry")}</Button>}
    {c.sources.filter(source => source.page?.next_cursor != null).map(source => <Button variant="ghost" key={source.stage_id} disabled={source.busy} onMouseDown={event => event.preventDefault()} onClick={() => void c.more(source)}>{t("composer.skills_more", {device: source.label})}</Button>)}
    {c.offline > 0 && <div className="dl-skill-notice">{t("composer.skills_offline", {count: c.offline})}</div>}
  </PopoverContent>;
}

export function SkillChips({skills, onRemove}: {skills?: SkillReference[]; onRemove?(skill: SkillReference): void}) {
  if (!skills?.length) return null;
  return <div className="dl-skill-chips">{skills.map(skill => <Tooltip key={skillKey(skill)}><TooltipTrigger asChild><Badge variant="secondary" tabIndex={0}><BoxIcon data-icon="inline-start" /><span>${skill.name}</span>{onRemove && <Button variant="ghost" size="icon-xs" aria-label={t("composer.remove_value0", {value0: skill.name})} onClick={() => onRemove(skill)}><XIcon /></Button>}</Badge></TooltipTrigger><TooltipContent><span className="dl-skill-path">{skill.stage_id}<br />{skill.path}</span></TooltipContent></Tooltip>)}</div>;
}
