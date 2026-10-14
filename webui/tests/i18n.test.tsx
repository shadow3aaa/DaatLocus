import { afterEach, describe, expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { Composer, modeLabel } from "../src/components/composer";
import { TooltipProvider } from "../src/components/ui/tooltip";
import { fieldLabel, toolLabel } from "../src/lib/tool-presentation";
import { browserLocale, defaultPreferences, readPreferences, savePreferences } from "../src/lib/preferences";
import i18n, { t } from "../src/lib/i18n";
import en from "../src/locales/en-US.json";
import zh from "../src/locales/zh-CN.json";

afterEach(async () => { await i18n.changeLanguage("zh-CN"); });
function flatten(value: Record<string, unknown>, prefix=""): Record<string,string> {
  return Object.fromEntries(Object.entries(value).flatMap(([key,entry])=>typeof entry==="string"?[[prefix+key,entry]]:Object.entries(flatten(entry as Record<string,unknown>,`${prefix}${key}.`))));
}
const render = () => renderToStaticMarkup(<TooltipProvider><Composer value="用户原文 stays unchanged" onValueChange={()=>{}} onSubmit={()=>{}} attachments={[]} onFiles={()=>{}} onRemoveAttachment={()=>{}} uploading={false} busy={false} working={true} onStop={()=>{}} canManage mode="ask" onModeChange={()=>{}} models={[]} modelName="" onModelChange={()=>{}} /></TooltipProvider>);

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
  test("static translation keys used by components resolve, including loading accessibility", async () => {
    const catalog=flatten(en);
    for await (const path of new Bun.Glob("src/**/*.{ts,tsx}").scan()) {
      if(path.includes("generated")) continue;
      const source=await Bun.file(path).text();
      for(const match of source.matchAll(/\bt\(\s*["']([^"']+)["']/g)) {
        const key=match[1];
        expect(Boolean(catalog[key] || catalog[`${key}_one`] || catalog[`${key}_other`])).toBe(true);
      }
    }
  });
  test("default follows the first supported browser language with English fallback", () => {
    expect(defaultPreferences.language).toBe("system");
    expect(browserLocale(["fr-FR","zh-TW","en-US"])).toBe("zh-CN");
    expect(browserLocale(["en-GB","zh-CN"])).toBe("en-US");
    expect(browserLocale(["de-DE"])).toBe("en-US");
  });
  test("saved manual language overrides the browser and automatic selection persists", () => {
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
  test("language changes refresh composer, tool labels and approval labels without touching source", async () => {
    await i18n.changeLanguage("en-US");
    expect(render()).toContain("Stop task");expect(render()).toContain("Queue message");
    expect(render()).toContain("用户原文 stays unchanged");
    expect(toolLabel("files__read_file")).toBe("Read file");expect(fieldLabel("path")).toBe("File");expect(modeLabel("ask")).toBe("Ask for approval");
    await i18n.changeLanguage("zh-CN");
    expect(render()).toContain("停止当前任务");expect(toolLabel("files__read_file")).toBe("读取文件");expect(modeLabel("ask")).toBe("请求批准");
  });
});
