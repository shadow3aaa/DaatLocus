import type { BundledTheme } from "shiki";

export const accentColors = { default: null, blue: "#2563EB", green: "#15803D", orange: "#C2410C", pink: "#BE185D", purple: "#7E22CE" } as const;
export type AccentColor = keyof typeof accentColors;
export const codeThemes = ["github", "vscode", "solarized", "vitesse"] as const;
export type CodeTheme = typeof codeThemes[number];
export type ColorScheme = "light" | "dark";
export type ThemePalette = { accent: AccentColor; background: string; foreground: string; codeTheme: CodeTheme };
export type AppearancePreferences = {
  light: ThemePalette; dark: ThemePalette;
  interfaceSize: number; contentSize: number; codeSize: number;
  motion: "system" | "reduce" | "full";
  fullWidth: boolean;
};
export function defaultAppearance(): AppearancePreferences {
  return {
    light: {accent: "default", background: "#FCFCFC", foreground: "#0D0D0D", codeTheme: "github"},
    dark: {accent: "default", background: "#0E0E0E", foreground: "#EDEDED", codeTheme: "github"},
    interfaceSize: 14, contentSize: 16, codeSize: 13, motion: "system", fullWidth: false,
  };
}
export function hexColor(value: unknown): string | null {
  return typeof value === "string" && /^#[0-9a-f]{6}$/i.test(value) ? value.toUpperCase() : null;
}
export function contrastRatio(background: string, foreground: string): number {
  function luminance(hex: string) {
    const channels = [1, 3, 5].map(index => {
      const channel = Number.parseInt(hex.slice(index, index + 2), 16) / 255;
      return channel <= .04045 ? channel / 12.92 : ((channel + .055) / 1.055) ** 2.4;
    });
    return channels[0] * .2126 + channels[1] * .7152 + channels[2] * .0722;
  }
  const a = luminance(background), b = luminance(foreground);
  return (Math.max(a, b) + .05) / (Math.min(a, b) + .05);
}
export function isCodeTheme(value: unknown): value is CodeTheme { return codeThemes.includes(value as CodeTheme); }
export function codeThemeFor(scheme: ColorScheme, theme: CodeTheme): BundledTheme {
  const themes: Record<CodeTheme, Record<ColorScheme, BundledTheme>> = {
    github: {light: "github-light-default", dark: "github-dark-default"},
    vscode: {light: "light-plus", dark: "dark-plus"},
    solarized: {light: "solarized-light", dark: "solarized-dark"},
    vitesse: {light: "vitesse-light", dark: "vitesse-dark"},
  };
  return themes[theme][scheme];
}
export function normalizeAppearance(value: unknown): AppearancePreferences {
  const defaults = defaultAppearance();
  if (!value || typeof value !== "object") return defaults;
  const saved = value as Record<string, unknown>;
  const size = (key: "interfaceSize" | "contentSize" | "codeSize", min: number, max: number) =>
    typeof saved[key] === "number" && Number.isFinite(saved[key]) ? Math.min(max, Math.max(min, Math.round(saved[key]))) : defaults[key];
  const palette = (scheme: ColorScheme): ThemePalette => {
    const candidate = saved[scheme] as Partial<ThemePalette> | null;
    const fallback = defaults[scheme];
    const background = hexColor(candidate?.background) ?? fallback.background;
    const foreground = hexColor(candidate?.foreground) ?? fallback.foreground;
    return {
      background: contrastRatio(background, foreground) >= 4.5 ? background : fallback.background,
      foreground: contrastRatio(background, foreground) >= 4.5 ? foreground : fallback.foreground,
      accent: candidate && Object.hasOwn(accentColors, candidate.accent ?? "") ? candidate.accent! : "default",
      codeTheme: isCodeTheme(candidate?.codeTheme) ? candidate.codeTheme : "github",
    };
  };
  return {light: palette("light"), dark: palette("dark"), interfaceSize: size("interfaceSize", 12, 18), contentSize: size("contentSize", 12, 24), codeSize: size("codeSize", 10, 20), motion: saved.motion === "reduce" || saved.motion === "full" ? saved.motion : "system", fullWidth: saved.fullWidth === true};
}

export function motionScrollBehavior(): ScrollBehavior {
  return document.documentElement.dataset.reducedMotion === "true" ? "auto" : "smooth";
}
