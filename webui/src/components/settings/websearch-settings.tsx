import { useEffect, useSyncExternalStore } from "react";
import { t } from "@/lib/i18n";
import { searchProviderNames, type SearchProvider, type WebSearchPreferences } from "@/lib/websearch";
import { SettingsGroup } from "./structure";
import { Picker, StatusNote, TextField } from "./controls";
import { Button } from "@/components/ui/button";

export function WebSearchSettings({preferences}: {preferences: WebSearchPreferences}) {
  const {settings,key,error,pending}=useSyncExternalStore(preferences.subscribe,preferences.getSnapshot);
  useEffect(()=>{const controller=new AbortController(); void preferences.load(controller.signal); return ()=>controller.abort();},[preferences]);
  const provider=settings?.provider;
  const configured=settings?.providers.find(item=>item.id===provider)?.credential_configured;
  return <>
    <StatusNote text={error} error />
    {error&&<Button variant="outline" disabled={pending} onClick={()=>void preferences.retry()}>{t("app.retry")}</Button>}
    <SettingsGroup>
      <Picker id="dl-websearch-provider" label={t("websearch.provider")} value={provider??""} disabled={!settings||pending||Boolean(error)} options={Object.entries(searchProviderNames).map(([value,label])=>({value,label}))} onChange={value=>{preferences.editKey(""); void preferences.save({provider:value as SearchProvider});}} />
      {provider==="opencode"?<p className="dl-tool-note">{t("websearch.console_login")}</p>:provider&&<>
        <TextField id="dl-websearch-key" label={t("provider_editor.api_key")} type="password" autoComplete="off" value={key} disabled={pending} placeholder={configured?t("websearch.key_saved"):""} onChange={event=>preferences.editKey(event.target.value)} onBlur={()=>{if(key)void preferences.save({provider,api_key:key});}} />
        {configured&&<Button variant="outline" disabled={pending} onPointerDown={event=>event.preventDefault()} onClick={()=>{preferences.editKey(""); void preferences.save({provider,api_key:""});}}>{t("websearch.clear_key")}</Button>}
      </>}
    </SettingsGroup>
  </>;
}
