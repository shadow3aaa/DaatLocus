import { accentColors, defaultAppearance, normalizeAppearance, type AppearancePreferences } from "./appearance";

export type LanguagePreference = "system" | "zh-CN" | "en-US";
export type Preferences = {
  theme: "light" | "dark" | "system";
  language: LanguagePreference;
  appearance: AppearancePreferences;
};
export type WebUiLocale = "zh-CN" | "en-US";
const KEY = "daat-locus.preferences";
export const defaultPreferences: Preferences = { theme: "system", language: "system", appearance: defaultAppearance() };

export function browserLocale(languages: readonly string[] = typeof navigator === "undefined" ? [] : navigator.languages?.length ? navigator.languages : [navigator.language]): WebUiLocale {
  for (const language of languages) {
    if (/^zh(?:-|$)/i.test(language)) return "zh-CN";
    if (/^en(?:-|$)/i.test(language)) return "en-US";
  }
  return "en-US";
}
export function resolveLanguage(value: LanguagePreference): WebUiLocale {
  return value === "system" ? browserLocale() : value;
}
export function readPreferences(): Preferences {
  try {
    const value = JSON.parse(localStorage.getItem(KEY) ?? "null");
    return {
      theme: ["light", "dark", "system"].includes(value?.theme) ? value.theme : "system",
      // The earlier client saved a fixed placeholder locale without a language selector.
      language: value?.version === 2 && ["en-US", "zh-CN"].includes(value?.language) ? value.language : "system",
      appearance: normalizeAppearance(value?.appearance),
    };
  } catch { return { ...defaultPreferences }; }
}
export function savePreferences(value: Preferences) {
  try { localStorage.setItem(KEY, JSON.stringify({version:2, ...value})); } catch { /* Keep in-memory preferences usable. */ }
}
export function applyPreferences(value: Preferences) {
  const dark = value.theme === "dark" || value.theme === "system" && matchMedia("(prefers-color-scheme: dark)").matches;
  document.documentElement.classList.toggle("dark", dark);
  document.documentElement.lang = resolveLanguage(value.language);
  document.documentElement.style.colorScheme = dark ? "dark" : "light";
  const root = document.documentElement;
  const appearance = value.appearance;
  const scheme = dark ? "dark" : "light";
  const palette = appearance[scheme];
  const defaults = defaultAppearance()[scheme];
  root.style.setProperty("--background", palette.background);
  root.style.setProperty("--foreground", palette.foreground);
  const custom = palette.background !== defaults.background || palette.foreground !== defaults.foreground;
  for (const [token, mix] of Object.entries({card: 3, popover: 5, secondary: 6, muted: 6, "settings-muted": 4, accent: 6, input: 14, "settings-border": 10, "history-muted": 17, "history-visible": 56, "history-hover": 100})) {
    if (custom) root.style.setProperty(`--${token}`, `color-mix(in srgb, ${palette.foreground} ${mix}%, ${palette.background})`);
    else root.style.removeProperty(`--${token}`);
  }
  for (const token of ["card-foreground", "popover-foreground", "secondary-foreground", "accent-foreground"]) {
    if (custom) root.style.setProperty(`--${token}`, palette.foreground);
    else root.style.removeProperty(`--${token}`);
  }
  if (custom) root.style.setProperty("--muted-foreground", `color-mix(in srgb, ${palette.foreground} 65%, ${palette.background})`);
  else root.style.removeProperty("--muted-foreground");
  const accent = accentColors[palette.accent];
  for (const [token, color] of Object.entries({"work-user": accent || (custom ? palette.foreground : null), "work-user-foreground": accent ? "#FFFFFF" : custom ? palette.background : null})) {
    if (color) root.style.setProperty(`--${token}`, color);
    else root.style.removeProperty(`--${token}`);
  }
  for (const token of ["primary", "ring", "chat-user"]) {
    if (accent) root.style.setProperty(`--${token}`, accent);
    else root.style.removeProperty(`--${token}`);
  }
  if (accent) root.style.setProperty("--primary-foreground", "#FFFFFF");
  else if (custom) { root.style.setProperty("--primary", palette.foreground); root.style.setProperty("--primary-foreground", palette.background); }
  else root.style.removeProperty("--primary-foreground");
  root.style.setProperty("--ui-font-size", `${appearance.interfaceSize}px`);
  root.style.fontSize = `${16 * appearance.interfaceSize / 14}px`;
  root.style.setProperty("--content-font-size", `${appearance.contentSize}px`);
  root.style.setProperty("--code-font-size", `${appearance.codeSize}px`);
  root.dataset.codeTheme = palette.codeTheme;
  root.dataset.fullWidth = String(appearance.fullWidth);
  root.dataset.reducedMotion = String(appearance.motion === "reduce" || appearance.motion === "system" && matchMedia("(prefers-reduced-motion: reduce)").matches);
}
