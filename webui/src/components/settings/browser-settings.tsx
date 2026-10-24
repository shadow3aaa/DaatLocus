import { t } from "@/lib/i18n";
import { useState } from "react";
import { useBrowserPreferences, searchEngines } from "@/lib/browser-settings";
import { browserLibrary, type BrowserPreferences } from "@/lib/browser-workspace";
import { SettingsGroup, SettingsRow, SettingsSection } from "./structure";
import { errorText, Picker, StatusNote } from "./controls";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogTitle, DialogDescription, DialogFooter } from "@/components/ui/dialog";
import { Spinner } from "@/components/ui/spinner";

export function BrowserSettings(){
  const [clear,setClear]=useState(false);
  const {value,loading,pending,error,update,retry}=useBrowserPreferences();
  const disabled=loading||pending;
  return <><StatusNote error text={error}/>{error&&<Button variant="outline" onClick={()=>void retry()}>{t("app.retry")}</Button>}<SettingsGroup className="mb-[26px]">
    <Picker id="dl-browser-search" label={t("browser_use.search_engine")} value={value.search_engine} disabled={disabled} options={Object.keys(searchEngines).map(key=>({value:key,label:{google:"Google",bing:"Bing",duckduckgo:"DuckDuckGo",baidu:"百度"}[key]!}))} onChange={search_engine=>void update({search_engine:search_engine as BrowserPreferences["search_engine"]})}/>
    <Picker id="dl-browser-language" label={t("browser_settings.language")} description={t("browser_settings.next_load")} value={value.language} disabled={disabled} options={[{value:"system",label:t("settings_page.detect_language")},{value:"zh-CN",label:t("common.chinese")},{value:"en-US",label:t("common.english")}]} onChange={language=>void update({language:language as BrowserPreferences["language"]})}/>
    <Picker id="dl-browser-zoom" label={t("browser_settings.zoom")} value={String(value.default_zoom)} disabled={disabled} options={[.5,.75,.9,1,1.1,1.25,1.5,1.75,2].map(zoom=>({value:String(zoom),label:`${Math.round(zoom*100)}%`}))} onChange={zoom=>void update({default_zoom:Number(zoom)})}/>
  </SettingsGroup>
    <SettingsSection title={t("browser_use.browsing_data")}><SettingsGroup>
      <SettingsRow label={t("browser_use.clear_data")} description={t("browser_settings.clear_summary")}>
        <Button variant="destructive" onClick={()=>setClear(true)}>{t("browser_settings.clear_action")}</Button>
      </SettingsRow>
    </SettingsGroup></SettingsSection>
    <ClearBrowserDataDialog open={clear} onOpenChange={setClear}/>
  </>;
}

export function ClearBrowserDataDialog({open,onOpenChange}:{open:boolean;onOpenChange:(open:boolean)=>void}){
  const [pending,setPending]=useState(false);
  const [error,setError]=useState<string|null>(null);
  function changeOpen(next:boolean){if(pending)return;setError(null);onOpenChange(next);}
  async function clear(){
    if(pending)return;
    setPending(true);setError(null);
    try{await browserLibrary("clear_data");onOpenChange(false);}
    catch(cause){setError(errorText(cause));}
    finally{setPending(false);}
  }
  return <Dialog open={open} onOpenChange={changeOpen}><DialogContent showCloseButton={!pending}>
    <DialogTitle>{t("browser_use.clear_data")}</DialogTitle>
    <DialogDescription>{t("browser_use.clear_data_description")}</DialogDescription>
    <StatusNote error text={error}/>
    <DialogFooter><Button variant="outline" disabled={pending} onClick={()=>changeOpen(false)}>{t("common.cancel")}</Button><Button variant="destructive" disabled={pending} onClick={()=>void clear()}>{pending&&<Spinner data-icon="inline-start"/>}{t("browser_use.clear_data")}</Button></DialogFooter>
  </DialogContent></Dialog>;
}
