import { describe, expect, test } from "bun:test";
import { browserLocale, defaultPreferences, readPreferences, savePreferences } from "../src/lib/preferences";
import en from "../src/locales/en-US.json";
import zh from "../src/locales/zh-CN.json";

function flatten(value: Record<string, unknown>, prefix=""): Record<string,string> {
  return Object.fromEntries(Object.entries(value).flatMap(([key,entry])=>typeof entry==="string"?[[prefix+key,entry]]:Object.entries(flatten(entry as Record<string,unknown>,`${prefix}${key}.`))));
}

describe("Chinese and English client preferences", () => {
  test("catalogs cover the same messages and interpolation variables", () => {
    const english=flatten(en), chinese=flatten(zh);
    expect(Object.keys(english).sort()).toEqual(Object.keys(chinese).sort());
    for (const key of Object.keys(english)) {
      expect([...english[key].matchAll(/{{(\w+)}}/g)].map(match=>match[1]).sort()).toEqual([...chinese[key].matchAll(/{{(\w+)}}/g)].map(match=>match[1]).sort());
      expect(chinese[key].length).toBeGreaterThan(0);
      expect(english[key].length).toBeGreaterThan(0);
    }
  });
  test("default follows the first supported browser language with English fallback", () => {
    expect(defaultPreferences.language).toBe("system");
    expect(browserLocale(["fr-FR","zh-TW","en-US"])).toBe("zh-CN");
    expect(browserLocale(["en-GB","zh-CN"])).toBe("en-US");
    expect(browserLocale(["de-DE"])).toBe("en-US");
  });
  test("language preferences round-trip through storage and legacy data resets to automatic", () => {
    const original=Object.getOwnPropertyDescriptor(globalThis,"localStorage");
    const values=new Map<string,string>();
    Object.defineProperty(globalThis,"localStorage",{configurable:true,value:{getItem:(key:string)=>values.get(key)??null,setItem:(key:string,value:string)=>values.set(key,value)}});
    try {
      expect(readPreferences().language).toBe("system");
      values.set("daat-locus.preferences",JSON.stringify({theme:"light",language:"zh-CN"}));
      expect(readPreferences()).toEqual({theme:"light",language:"system"});
      savePreferences({theme:"dark",language:"zh-CN"});
      expect(readPreferences()).toEqual({theme:"dark",language:"zh-CN"});
      savePreferences({theme:"system",language:"system"});
      expect(readPreferences().language).toBe("system");
    } finally { if(original)Object.defineProperty(globalThis,"localStorage",original);else Reflect.deleteProperty(globalThis,"localStorage"); }
  });
});
