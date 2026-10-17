import { t } from "@/lib/i18n";
import type { Preferences } from "@/lib/preferences";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";
import { SettingsGroup, SettingsRow, SettingsSection } from "./structure";

export function AppearanceSettings({preferences, onChange}: {preferences: Preferences; onChange(value: Preferences): void}) {
  return <SettingsSection title={t("settings_page.visual_style")}><SettingsGroup><SettingsRow label={t("settings_page.mode")}>
      <ToggleGroup type="single" aria-label={t("settings_dialog.theme")} className="dl-theme-options" value={preferences.theme} onValueChange={value => {if (value) onChange({...preferences, theme: value as Preferences["theme"]});}}>
        {(["system", "light", "dark"] as const).map(theme => <ToggleGroupItem key={theme} value={theme} aria-label={t(`settings_dialog.${theme}`)} title={t(`settings_dialog.${theme}`)}><ThemePreview theme={theme} /></ToggleGroupItem>)}
      </ToggleGroup>
    </SettingsRow></SettingsGroup></SettingsSection>;
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
