import { t } from "@/lib/i18n";
import { useEffect, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { SettingsSection, SettingsGroup, SettingsRow } from "./structure";
import { Choices, TextField, StatusNote, errorText } from "./controls";
import { AutoSave } from "@/lib/auto-save";
import { validConnection } from "@/lib/device-settings";
import * as api from "@/lib/stage-api";
import { PublicLoginStatusView } from "./public-login-status";
import { DeviceList } from "./device-list";
import { ResetWork } from "./reset-work";
export function AccountEditor({ onChanged }: {onChanged: () => void}) {
  const [info,setInfo]=useState<api.StageInfo|null>(null);const [devices,setDevices]=useState<api.Device[]>([]);
  const [publication,setPublication]=useState<api.PublicLoginStatus|null>(null);
  const [publicationError,setPublicationError]=useState(false);
  const devicePages=useRef(1);
  const deviceQueue=useRef<Promise<unknown>>(Promise.resolve());
  const nextDevice=useRef<string|null>(null);
  const [devicesLoading,setDevicesLoading]=useState(true);
  const [deviceCursor,setDeviceCursor]=useState<string|null>(null);
  const [deviceError,setDeviceError]=useState<string|null>(null);
  const [validation,setValidation]=useState<string|null>(null);
  const [retry,setRetry]=useState(0);
  const [settings,setSettings]=useState<api.DeviceSettings|null>(null);const [error,setError]=useState<string|null>(null);
  const notify=useRef(onChanged);notify.current=onChanged;
  const mounted=useRef(true);const pending=useRef<api.DevicePreferences>({});
  const saver=useRef<AutoSave<api.DevicePreferences>|null>(null);
  if (!saver.current) saver.current=new AutoSave(async patch=>{const saved=await api.saveDeviceSettings(patch);if(mounted.current)setInfo(current=>current?{...current,settings:saved}:current);if(pending.current===patch)pending.current={};},cause=>{if(mounted.current)setError(errorText(cause));},()=>{if(mounted.current)setError(null);notify.current();});
  useEffect(()=>{
    mounted.current=true;const controller=new AbortController();
    void api.getStage({signal:controller.signal}).then(value=>{if(controller.signal.aborted)return;setInfo(value);setSettings(value.settings);setPublication(value.public_login ?? null);}).catch(cause=>{if(!controller.signal.aborted)setError(errorText(cause));});
    return()=>{mounted.current=false;controller.abort();void saver.current?.flush();};
  },[retry]);
  useEffect(()=>{
    const controller=new AbortController();
    let timer:ReturnType<typeof setTimeout>|undefined;
    setDevicesLoading(true);
    async function refresh() {
      const task=deviceQueue.current.catch(()=>{}).then(async()=>{
      if(controller.signal.aborted)return;
      try {
        const [status, page] = await Promise.all([api.getStageStatus({signal:controller.signal}), (async () => {
          const devices=new Map<string,api.Device>();
          let after:string|undefined;
          for(let pageIndex=0;pageIndex<devicePages.current;pageIndex++) {
            const page=await api.getDevices({after,signal:controller.signal});
            for(const device of page.items)devices.set(device.stage_id,device);
            after=page.next_cursor??undefined;
            if(!after)break;
          }
          return {devices:[...devices.values()],after};
        })()]);
        if(controller.signal.aborted)return;
        setPublication(status.public_login ?? null);setPublicationError(false);
        setDevices(page.devices);nextDevice.current=page.after??null;setDeviceCursor(nextDevice.current);setDeviceError(null);
      } catch(cause) {
        if(!controller.signal.aborted){setDeviceError(errorText(cause));setPublicationError(true);}
      } finally {
        if(!controller.signal.aborted) {setDevicesLoading(false);timer=setTimeout(()=>void refresh(),5000);}
      }
      });
      deviceQueue.current=task;
      await task;
    }
    void refresh();
    return()=>{controller.abort();clearTimeout(timer);};
  },[]);
  function moreDevices(signal:AbortSignal){
    const task=deviceQueue.current.catch(()=>{}).then(async()=>{
      const after=nextDevice.current;
      if(!after||signal.aborted||!mounted.current)return;
      const page=await api.getDevices({after,signal});
      if(signal.aborted||!mounted.current)return;
      setDevices(previous=>[...previous,...page.items.filter(item=>!previous.some(old=>old.stage_id===item.stage_id))]);
      nextDevice.current=page.next_cursor;setDeviceCursor(nextDevice.current);devicePages.current++;
    });
    deviceQueue.current=task;
    return task;
  }
  function save(patch:api.DevicePreferences){setError(null);pending.current={...pending.current,...patch};saver.current?.queue(pending.current);}
  function change(key:"label"|"relay_origin",value:string){setSettings(current=>current?{...current,[key]:value||null}:current);setValidation(null);if(key==="label"&&!value.trim()||key==="relay_origin"&&!validConnection(value))return;if(key==="relay_origin"&&settings?.mode==="relay"&&!value)return;save({[key]:value||null,...(key==="relay_origin"?{mode:settings?.mode}: {})} as api.DevicePreferences);}
  function changeMode(mode:api.Connectivity){
    setSettings(current=>current?{...current,mode}:current);setValidation(null);
    save({mode});
  }
  return <div className="dl-account-settings"><StatusNote text={error} error/>
    {error&&<Button variant="outline" onClick={()=>{setError(null);if(!info)setRetry(value=>value+1);else void saver.current?.flush();}}>{t("app.retry")}</Button>}
    {info?.name&&<SettingsSection title={t("app.account")}><SettingsGroup><SettingsRow label={t("access_gate.account_name")}><span>{info.name}</span></SettingsRow></SettingsGroup></SettingsSection>}
    {settings?.mode==="relay"&&<DeviceList local={info?{id:info.stage_id,name:info.settings.label||info.stage_id.slice(0,8)}:null} devices={devices} loading={devicesLoading} error={deviceError} cursor={deviceCursor} onMore={moreDevices}/>}
    {settings&&<SettingsSection title={t("account_editor.this_device")}><SettingsGroup>
      <TextField id="dl-device-label" label={t("account_editor.name")} value={settings.label??""} maxLength={100} aria-invalid={!settings.label?.trim()} onBlur={()=>{if(!settings.label?.trim())setValidation(t("account_editor.name_required"));}} onChange={event=>change("label",event.target.value)}/>
      <Choices id="dl-connectivity" label={t("account_editor.connectivity")} value={settings.mode} onChange={mode=>changeMode(mode as api.Connectivity)} options={[{value:"local",label:t("account_editor.local_mode")},{value:"relay",label:t("account_editor.relay_mode")}]} />
      {settings.mode==="relay"&&<TextField id="dl-device-relay" label={t("access_gate.public_connection_service")} value={settings.relay_origin??""} placeholder="wss://" aria-invalid={!validConnection(settings.relay_origin??"")} onBlur={()=>{if(!validConnection(settings.relay_origin??""))setValidation(t("account_editor.invalid_connection"));}} onChange={event=>change("relay_origin",event.target.value)}/>}
      {settings.mode==="relay"&&<SettingsRow label={t("account_editor.public_login_status")}><PublicLoginStatusView status={publicationError ? null : publication} /></SettingsRow>}
      <StatusNote text={validation} error/>
    </SettingsGroup></SettingsSection>}
    {info?.account_id && <ResetWork />}
  </div>;
}
