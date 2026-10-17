import { describe, expect, test } from "bun:test";
import { contrastRatio, defaultAppearance, hexColor, normalizeAppearance } from "../src/lib/appearance";
import { highlightCodeWithShiki } from "../src/lib/shiki-highlight";
import { defaultPreferences, readPreferences, savePreferences } from "../src/lib/preferences";

describe("local appearance preferences", () => {
  test("old and malformed settings remain readable without applying arbitrary CSS", () => {
    expect(normalizeAppearance(null)).toEqual(defaultAppearance());
    const value = normalizeAppearance({light: {background:"url(https://example.test)", foreground:"#fcfcfc", accent:"toString", codeTheme:"unknown"}, interfaceSize: 100, contentSize: NaN, codeSize: 3, motion:"invalid", fullWidth:"true"});
    expect(value.light).toEqual(defaultAppearance().light);
    expect(value.interfaceSize).toBe(18);
    expect(value.contentSize).toBe(16);
    expect(value.codeSize).toBe(10);
    expect(value.motion).toBe("system");
    expect(value.fullWidth).toBe(false);
    expect(hexColor("#a0B1c2")).toBe("#A0B1C2");
    expect(contrastRatio("#000000", "#FFFFFF")).toBeCloseTo(21);
  });
  test("light and dark palettes and reading preferences survive a reload independently", () => {
    const original = Object.getOwnPropertyDescriptor(globalThis, "localStorage");
    let stored = "";
    Object.defineProperty(globalThis, "localStorage", {configurable:true, value:{getItem:()=>stored, setItem:(_key:string,value:string)=>{stored=value;}}});
    try {
      const appearance = defaultAppearance();
      appearance.light = {...appearance.light, background:"#FFF8ED", accent:"green", codeTheme:"solarized"};
      appearance.dark = {...appearance.dark, accent:"purple", codeTheme:"vscode"};
      appearance.codeSize = 17; appearance.fullWidth = true; appearance.motion = "reduce";
      savePreferences({...defaultPreferences, appearance});
      expect(readPreferences().appearance).toEqual(appearance);
    } finally { if (original) Object.defineProperty(globalThis,"localStorage",original); else Reflect.deleteProperty(globalThis,"localStorage"); }
  });
  test("all offered code themes produce complete source and theme-specific palettes", async () => {
    const source = 'const greeting = "Hello, world!";\nconsole.log(greeting);';
    const palettes = new Set<string>();
    for (const scheme of ["light", "dark"] as const) {
      for (const theme of ["github", "vscode", "solarized", "vitesse"] as const) {
        const result = await highlightCodeWithShiki(source, "javascript", scheme, theme);
        expect(result).not.toBeNull();
        expect(result!.lines.map(line => line.map(token=>token.content).join("")).join("\n")).toBe(source);
        expect(result!.background).toMatch(/^#/);
        palettes.add(JSON.stringify(result));
      }
    }
    expect(palettes.size).toBe(8);
  });
});
