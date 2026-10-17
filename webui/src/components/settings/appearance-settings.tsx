import { useEffect, useState, useSyncExternalStore } from "react";
import { ChevronDownIcon, RotateCcwIcon } from "lucide-react";
import { t } from "@/lib/i18n";
import type { Preferences } from "@/lib/preferences";
import { accentColors, codeThemes, contrastRatio, defaultAppearance, hexColor, type AppearancePreferences, type ColorScheme, type ThemePalette } from "@/lib/appearance";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";
import { CollapsibleContent } from "@/components/ui/collapsible";
import { Select, SelectContent, SelectGroup, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { CodeBlock } from "@/components/markdown";
import { SettingsGroup, SettingsRow, SettingsSection } from "./structure";

const subscribeScheme = (callback: () => void) => { const media = matchMedia("(prefers-color-scheme: dark)"); media.addEventListener("change", callback); return () => media.removeEventListener("change", callback); };
const systemDark = () => matchMedia("(prefers-color-scheme: dark)").matches;
const themeNames = {github: "GitHub", vscode: "VS Code", solarized: "Solarized", vitesse: "Vitesse"};

export function AppearanceSettings({preferences, onChange}: {preferences: Preferences; onChange(value: Preferences): void}) {
  const dark = useSyncExternalStore(subscribeScheme, systemDark);
  const scheme: ColorScheme = preferences.theme === "system" ? dark ? "dark" : "light" : preferences.theme;
  const appearance = preferences.appearance;
  const palette = appearance[scheme];
  const [advanced, setAdvanced] = useState(false);
  const [resetRevision, setResetRevision] = useState(0);
  function change(patch: Partial<AppearancePreferences>) { onChange({...preferences, appearance: {...appearance, ...patch}}); }
  function changePalette(patch: Partial<ThemePalette>) { change({[scheme]: {...palette, ...patch}}); }
  function changeBackground(background: string) {
    let foreground = palette.foreground;
    if (contrastRatio(background, foreground) < 4.5) foreground = contrastRatio(background, "#0D0D0D") >= 4.5 ? "#0D0D0D" : "#FCFCFC";
    changePalette({background, foreground});
  }
  return <div className="dl-appearance-settings">
    <SettingsSection title={t("settings_page.visual_style")}><SettingsGroup><SettingsRow label={t("settings_page.mode")}>
      <ToggleGroup type="single" aria-label={t("settings_dialog.theme")} className="dl-theme-options" value={preferences.theme} onValueChange={value => {if (value) onChange({...preferences, theme: value as Preferences["theme"]});}}>
        {(["system", "light", "dark"] as const).map(theme => <ToggleGroupItem key={theme} value={theme} aria-label={t(`settings_dialog.${theme}`)} title={t(`settings_dialog.${theme}`)}><ThemePreview theme={theme} /></ToggleGroupItem>)}
      </ToggleGroup>
    </SettingsRow></SettingsGroup>
    <SettingsGroup className="dl-palette-settings">
      <SettingsRow id="dl-code-theme" label={t("appearance.code_theme")}>
        <Select value={palette.codeTheme} onValueChange={value => changePalette({codeTheme: value as ThemePalette["codeTheme"]})}><SelectTrigger id="dl-code-theme"><span className="dl-code-theme-icon" aria-hidden="true">Aa</span><SelectValue /></SelectTrigger><SelectContent><SelectGroup>{codeThemes.map(theme => <SelectItem key={theme} value={theme}>{themeNames[theme]}</SelectItem>)}</SelectGroup></SelectContent></Select>
      </SettingsRow>
      <SettingsRow id="dl-accent-color" label={t("appearance.accent")}><Select value={palette.accent} onValueChange={value => changePalette({accent: value as ThemePalette["accent"]})}><SelectTrigger id="dl-accent-color"><SelectValue /></SelectTrigger><SelectContent><SelectGroup>{Object.entries(accentColors).map(([color, hex]) => <SelectItem key={color} value={color}><span className="dl-color-option"><span className="dl-color-dot" style={{background: hex ?? "var(--foreground)"}} />{t(`appearance.colors.${color}`)}</span></SelectItem>)}</SelectGroup></SelectContent></Select></SettingsRow>
      <ColorSetting key={`${scheme}-background-${resetRevision}`} id="dl-background-color" label={t("appearance.background")} value={palette.background} onChange={changeBackground} />
      <ColorSetting key={`${scheme}-foreground-${resetRevision}`} id="dl-foreground-color" label={t("appearance.foreground")} value={palette.foreground} onChange={foreground => changePalette({foreground})} valid={foreground => contrastRatio(palette.background, foreground) >= 4.5} />
    </SettingsGroup></SettingsSection>
    <section className="dl-appearance-advanced">
      <Button variant="ghost" className="dl-appearance-disclosure" aria-expanded={advanced} aria-controls="dl-appearance-advanced-content" onClick={() => setAdvanced(!advanced)}>{t("appearance.advanced")}<ChevronDownIcon data-icon="inline-end" /></Button>
      <div id="dl-appearance-advanced-content"><CollapsibleContent open={advanced} className="dl-appearance-advanced-content">
        <SettingsGroup>
          <FontSize id="dl-interface-size" label={t("appearance.interface_size")} value={appearance.interfaceSize} min={12} max={18} onChange={interfaceSize => change({interfaceSize})} />
          <FontSize id="dl-content-size" label={t("appearance.content_size")} value={appearance.contentSize} min={12} max={24} onChange={contentSize => change({contentSize})} />
          <FontSize id="dl-code-size" label={t("appearance.code_size")} value={appearance.codeSize} min={10} max={20} onChange={codeSize => change({codeSize})} />
        </SettingsGroup>
        <SettingsGroup>
          <SettingsRow label={t("appearance.reduced_motion")}><ToggleGroup type="single" className="dl-settings-options dl-motion-options" value={appearance.motion} aria-label={t("appearance.reduced_motion")} onValueChange={motion => {if (motion) change({motion: motion as AppearancePreferences["motion"]});}}>{(["system", "reduce", "full"] as const).map(value => <ToggleGroupItem key={value} value={value}>{t(`appearance.motion.${value}`)}</ToggleGroupItem>)}</ToggleGroup></SettingsRow>
          <SettingsRow id="dl-full-width" label={t("appearance.full_width")}><Switch id="dl-full-width" checked={appearance.fullWidth} onCheckedChange={fullWidth => change({fullWidth})} /></SettingsRow>
        </SettingsGroup>
      </CollapsibleContent></div>
    </section>
    <SettingsSection title={t("appearance.preview")}><div className="dl-appearance-preview"><p>{t("appearance.preview_text")}</p><CodeBlock language="javascript" code={'const greeting = "Hello, world!";\nconsole.log(greeting);'} /></div></SettingsSection>
    <Button variant="ghost" className="dl-appearance-reset" onClick={() => {setResetRevision(value => value + 1);onChange({...preferences, theme: "system", appearance: defaultAppearance()});}}><RotateCcwIcon data-icon="inline-start" />{t("appearance.reset")}</Button>
  </div>;
}

function ColorSetting({id, label, value, onChange, valid = () => true}: {id: string; label: string; value: string; onChange(value: string): void; valid?(value: string): boolean}) {
  const [draft, setDraft] = useState(value);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {setDraft(value); setError(null);}, [value]);
  function commit(input: string) {
    const color = hexColor(input);
    if (!color) {setError(t("appearance.invalid_color")); return;}
    if (!valid(color)) {setError(t("appearance.low_contrast")); return;}
    setError(null); setDraft(color); onChange(color);
  }
  return <SettingsRow id={id} label={label} className="dl-color-setting"><div className="dl-color-control"><div className="dl-color-input"><input type="color" value={value} aria-label={t("appearance.color_picker", {label})} onChange={event => commit(event.target.value)} /><Input id={id} value={draft} spellCheck={false} maxLength={7} aria-invalid={!!error} aria-describedby={error ? `${id}-error` : undefined} onChange={event => {setDraft(event.target.value);setError(null);}} onBlur={() => commit(draft)} onKeyDown={event => {if (event.key === "Enter") commit(draft);}} /></div>{error && <span className="dl-color-error" id={`${id}-error`} role="alert">{error}</span>}</div></SettingsRow>;
}

function FontSize({id, label, value, min, max, onChange}: {id: string; label: string; value: number; min: number; max: number; onChange(value: number): void}) {
  const [draft, setDraft] = useState(String(value));
  useEffect(() => setDraft(String(value)), [value]);
  function commit() {const number = Number(draft); const next = draft.trim() && Number.isFinite(number) ? Math.min(max, Math.max(min, Math.round(number))) : value;setDraft(String(next));if (next !== value) onChange(next);}
  return <SettingsRow id={id} label={label}><div className="dl-font-size-input"><Input id={id} type="number" min={min} max={max} step={1} value={draft} onChange={event => {setDraft(event.target.value);const number = Number(event.target.value);if (Number.isInteger(number) && number >= min && number <= max) onChange(number);}} onBlur={commit} onKeyDown={event => {if (event.key === "Enter") commit();}} /><span>px</span></div></SettingsRow>;
}

function ThemePreview({theme}: {theme: Preferences["theme"]}) {
  return <span className="dl-theme-preview" data-theme={theme} aria-hidden="true">{theme === "system" ? <><ThemeMiniature half dark={false} /><ThemeMiniature half dark /></> : <ThemeMiniature dark={theme === "dark"} />}</span>;
}

function ThemeMiniature({dark, half = false}: {dark: boolean; half?: boolean}) {
  const ink = dark ? "#fff" : "#000";
  return <svg viewBox={`0 0 ${half ? 40 : 80} 60`} fill="none" xmlns="http://www.w3.org/2000/svg">
    <rect width="80" height="60" rx={half ? 0 : 8} fill={dark ? "#262626" : "#f4f4f4"} />
    <rect className="dl-theme-miniature-window" x="8" y="8" width="64" height="44" rx="4" fill={dark ? "#0d0d0d" : "#fff"} stroke={ink} strokeOpacity=".1" strokeWidth=".5" />
    <path d="M8 12a4 4 0 0 1 4-4h8v44h-8a4 4 0 0 1-4-4Z" fill={ink} fillOpacity={dark ? ".2" : ".03"} />
    <image href="/logo-mark.svg" x="11" y="11" width="6" height="6" opacity=".5" style={{filter: dark ? "invert(1)" : undefined}} />
    <g fill="currentColor"><rect x="38" y="12" width="30" height="2" rx="1" /><rect x="29" y="32" width="39" height="2" rx="1" /><rect x="45" y="36" width="23" height="2" rx="1" /><circle cx="65" cy="45" r="2" /></g>
    <g fill={ink} opacity={dark ? ".3" : ".1"}><rect x="24" y="18" width="30" height="2" rx="1" /><rect x="24" y="22" width="18" height="2" rx="1" /><rect x="24" y="26" width="36" height="2" rx="1" /></g>
    <rect x="23.75" y="41.75" width="44.5" height="6.5" rx="3.25" stroke={ink} strokeOpacity={dark ? ".3" : ".1"} strokeWidth=".5" />
  </svg>;
}
