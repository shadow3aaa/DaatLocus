export type LanguagePreference = "system" | "zh-CN" | "en-US";
export type Preferences = {
  theme: "light" | "dark" | "system";
  language: LanguagePreference;
};
export type WebUiLocale = "zh-CN" | "en-US";
const KEY = "daat-locus.preferences";
export const defaultPreferences: Preferences = { theme: "system", language: "system" };

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
}
