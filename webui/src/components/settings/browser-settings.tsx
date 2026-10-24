import { t } from "@/lib/i18n";
import { useBrowserPreferences, searchEngines } from "@/lib/browser-settings";
import type { BrowserPreferences } from "@/lib/browser-workspace";
import { SettingsGroup } from "./structure";
import { Picker, StatusNote } from "./controls";
import { Button } from "@/components/ui/button";

export function BrowserSettings(){
  const {value,loading,pending,error,update,retry}=useBrowserPreferences();
  const disabled=loading||pending;
  return <><StatusNote error text={error}/>{error&&<Button variant="outline" onClick={()=>void retry()}>{t("app.retry")}</Button>}<SettingsGroup>
    <Picker id="dl-browser-search" label={t("browser_use.search_engine")} value={value.search_engine} disabled={disabled} options={Object.keys(searchEngines).map(key=>({value:key,label:{google:"Google",bing:"Bing",duckduckgo:"DuckDuckGo",baidu:"百度"}[key]!}))} onChange={search_engine=>void update({search_engine:search_engine as BrowserPreferences["search_engine"]})}/>
    <Picker id="dl-browser-language" label={t("browser_settings.language")} description={t("browser_settings.next_load")} value={value.language} disabled={disabled} options={[{value:"system",label:t("settings_page.detect_language")},{value:"zh-CN",label:t("common.chinese")},{value:"en-US",label:t("common.english")}]} onChange={language=>void update({language:language as BrowserPreferences["language"]})}/>
    <Picker id="dl-browser-zoom" label={t("browser_settings.zoom")} value={String(value.default_zoom)} disabled={disabled} options={[.5,.75,.9,1,1.1,1.25,1.5,1.75,2].map(zoom=>({value:String(zoom),label:`${Math.round(zoom*100)}%`}))} onChange={zoom=>void update({default_zoom:Number(zoom)})}/>
  </SettingsGroup></>;
}
