import i18n from "i18next";
import { initReactI18next } from "react-i18next";
import en from "@/locales/en-US.json";
import zh from "@/locales/zh-CN.json";
import { readPreferences, resolveLanguage, type LanguagePreference, type WebUiLocale } from "./preferences";
export type { WebUiLocale } from "./preferences";
export const WEBUI_LOCALES = ["en-US", "zh-CN"] as const;
export const DEFAULT_WEBUI_LOCALE: WebUiLocale = "en-US";
export function normalizeWebUiLocale(locale: string | null | undefined): WebUiLocale {
  return locale?.toLowerCase().startsWith("zh") ? "zh-CN" : "en-US";
}
function applyDocumentLanguage(locale: WebUiLocale) {
  if (typeof document !== "undefined") document.documentElement.lang = locale;
}
const initialLanguage = resolveLanguage(readPreferences().language);
applyDocumentLanguage(initialLanguage);
void i18n.use(initReactI18next).init({
  resources: {"en-US": {translation: en}, "zh-CN": {translation: zh}},
  lng: initialLanguage,
  fallbackLng: DEFAULT_WEBUI_LOCALE,
  supportedLngs: [...WEBUI_LOCALES],
  returnNull: false,
  interpolation: { escapeValue: false },
  react: { useSuspense: false },
});
i18n.on("languageChanged", language => applyDocumentLanguage(normalizeWebUiLocale(language)));
export function getCurrentWebUiLanguage(): WebUiLocale {
  return normalizeWebUiLocale(i18n.resolvedLanguage ?? i18n.language);
}
export async function setWebUiLanguage(preference: LanguagePreference) {
  const locale = resolveLanguage(preference);
  applyDocumentLanguage(locale);
  if (getCurrentWebUiLanguage() !== locale) await i18n.changeLanguage(locale);
  return locale;
}
export function t(key: string, values?: Record<string, unknown>): string {
  return i18n.t(key, values);
}
export function apiErrorText(code: string): string {
  return t(i18n.exists(`errors.${code}`) ? `errors.${code}` : "errors.request_failed");
}
export default i18n;
