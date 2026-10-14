import { t } from "@/lib/i18n";
import { useEffect, useRef, useState } from "react";
import { PlusIcon, XIcon } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { SettingsSection, SettingsGroup, SettingsRow } from "./structure";
import { Picker, TextField, StatusNote, errorText } from "./controls";
import { AutoSave } from "@/lib/auto-save";
import { sshTargets, validConnection, type SshDraft } from "@/lib/device-settings";
import * as api from "@/lib/stage-api";
import { PublicLoginStatusView } from "./public-login-status";
import { DeviceList } from "./device-list";
export function AccountEditor({ onChanged }: {onChanged: () => void}) {
  const [info,setInfo]=useState<api.StageInfo|null>(null);const [devices,setDevices]=useState<api.Device[]>([]);
  const [publication,setPublication]=useState<api.PublicLoginStatus|null>(null);
  const [publicationError,setPublicationError]=useState(false);
  const [devicePages,setDevicePages]=useState(1);
  const [devicesLoading,setDevicesLoading]=useState(true);
  const [deviceCursor,setDeviceCursor]=useState<string|null>(null);
  const [deviceError,setDeviceError]=useState<string|null>(null);
  const [validation,setValidation]=useState<string|null>(null);
  const [retry,setRetry]=useState(0);
  const [settings,setSettings]=useState<api.DeviceSettings|null>(null);const [ssh,setSsh]=useState<SshDraft[]>([]);const [error,setError]=useState<string|null>(null);
  const notify=useRef(onChanged);notify.current=onChanged;
  const mounted=useRef(true);const pending=useRef<api.DevicePreferences>({});const sshRef=useRef<SshDraft[]>([]);
  const saver=useRef<AutoSave<api.DevicePreferences>|null>(null);
  if (!saver.current) saver.current=new AutoSave(async patch=>{const saved=await api.saveDeviceSettings(patch);if(mounted.current)setInfo(current=>current?{...current,settings:saved}:current);if(pending.current===patch)pending.current={};},cause=>{if(mounted.current)setError(errorText(cause));},()=>{if(mounted.current)setError(null);notify.current();});
  useEffect(()=>{
    mounted.current=true;const controller=new AbortController();
    void api.getStage({signal:controller.signal}).then(value=>{if(controller.signal.aborted)return;setInfo(value);setSettings(value.settings);setPublication(value.public_login ?? null);const rows=Object.entries(value.settings.ssh).map(([stage,target])=>({key:crypto.randomUUID(),stage,host:target.host,user:target.user,ssh_port:String(target.ssh_port),stage_port:String(target.stage_port)}));sshRef.current=rows;setSsh(rows);}).catch(cause=>{if(!controller.signal.aborted)setError(errorText(cause));});
    return()=>{mounted.current=false;controller.abort();void saver.current?.flush();};
  },[retry]);
  useEffect(()=>{
    const controller=new AbortController();
    let timer:ReturnType<typeof setTimeout>|undefined;
    setDevicesLoading(true);
    async function refresh() {
      try {
        const [status, page] = await Promise.all([api.getStageStatus({signal:controller.signal}), (async () => {
          const devices=new Map<string,api.Device>();
          let after:string|undefined;
          for(let pageIndex=0;pageIndex<devicePages;pageIndex++) {
            const page=await api.getDevices({after,signal:controller.signal});
            for(const device of page.items)devices.set(device.stage_id,device);
            after=page.next_cursor??undefined;
            if(!after)break;
          }
          return {devices:[...devices.values()],after};
        })()]);
        if(controller.signal.aborted)return;
        setPublication(status.public_login ?? null);setPublicationError(false);
        setDevices(page.devices);setDeviceCursor(page.after??null);setDeviceError(null);
      } catch(cause) {
        if(!controller.signal.aborted){setDeviceError(errorText(cause));setPublicationError(true);}
      } finally {
        if(!controller.signal.aborted) {setDevicesLoading(false);timer=setTimeout(()=>void refresh(),5000);}
      }
    }
    void refresh();
    return()=>{controller.abort();clearTimeout(timer);};
  },[devicePages]);
  function save(patch:api.DevicePreferences){setError(null);pending.current={...pending.current,...patch};saver.current?.queue(pending.current);}
  function change(key:"label"|"signaling_origin",value:string){setSettings(current=>current?{...current,[key]:value||null}:current);setValidation(null);if(key==="label"&&!value.trim()||key==="signaling_origin"&&!validConnection(value))return;save({[key]:value||null} as api.DevicePreferences);}
  function updateSsh(rows:SshDraft[]){sshRef.current=rows;setSsh(rows);const targets=sshTargets(rows);if(targets)save({ssh:targets});}
  function editSsh(key:string,patch:Partial<SshDraft>){updateSsh(sshRef.current.map(row=>row.key===key?{...row,...patch}:row));}
  function removeSsh(key:string){updateSsh(sshRef.current.filter(row=>row.key!==key));}
  const peers=devices.filter(device=>device.stage_id!==info?.stage_id);
  const available=peers.filter(device=>!ssh.some(row=>row.stage===device.stage_id));
  return <div className="dl-account-settings"><StatusNote text={error} error/>
    {error&&<Button variant="outline" onClick={()=>{setError(null);if(!info)setRetry(value=>value+1);else void saver.current?.flush();}}>{t("app.retry")}</Button>}
    {info?.name&&<SettingsSection title={t("app.account")}><SettingsGroup><SettingsRow label={t("access_gate.account_name")}><span>{info.name}</span></SettingsRow></SettingsGroup></SettingsSection>}
    <DeviceList local={info?{id:info.stage_id,name:info.settings.label||info.stage_id.slice(0,8)}:null} devices={devices} loading={devicesLoading} error={deviceError} hasMore={Boolean(deviceCursor)} onMore={()=>setDevicePages(pages=>pages+1)}/>
    {settings&&<SettingsSection title={t("account_editor.this_device")}><SettingsGroup>
      <TextField id="dl-device-label" label={t("account_editor.name")} value={settings.label??""} maxLength={100} aria-invalid={!settings.label?.trim()} onBlur={()=>{if(!settings.label?.trim())setValidation(t("account_editor.name_required"));}} onChange={event=>change("label",event.target.value)}/>
      <TextField id="dl-device-signaling" label={t("access_gate.public_connection_service")} value={settings.signaling_origin??""} placeholder="wss://" aria-invalid={!validConnection(settings.signaling_origin??"")} onBlur={()=>{if(!validConnection(settings.signaling_origin??""))setValidation(t("account_editor.invalid_connection"));}} onChange={event=>change("signaling_origin",event.target.value)}/>
      <SettingsRow label={t("account_editor.public_login_status")}><PublicLoginStatusView status={publicationError ? null : publication} /></SettingsRow>
      <StatusNote text={validation} error/>
    </SettingsGroup></SettingsSection>}
    {settings&&<SettingsSection title="SSH" action={<Button variant="ghost" size="sm" disabled={!available.length||ssh.some(row=>!row.stage)} aria-label={t("account_editor.add_ssh")} onClick={()=>{const rows=[...sshRef.current,{key:crypto.randomUUID(),stage:"",host:"",user:null,ssh_port:"22",stage_port:""}];sshRef.current=rows;setSsh(rows);}}><PlusIcon data-icon="inline-start"/>{t("composer.add")}</Button>}>
      {!peers.length&&!ssh.length&&<p className="dl-settings-caption">{t("account_editor.no_ssh_devices")}</p>}
      {ssh.map(row=><SettingsGroup key={row.key}><Picker id={`ssh-${row.key}-device`} label={t("account_editor.device")} value={row.stage} options={[...available,...(row.stage ? [peers.find(device=>device.stage_id===row.stage) ?? {stage_id:row.stage,label:row.stage.slice(0,8)}] : [])].map(device=>({value:device.stage_id,label:device.label||device.stage_id.slice(0,8)}))} onChange={stage=>editSsh(row.key,{stage})} action={<Button variant="ghost" size="icon-sm" aria-label={t("account_editor.remove_ssh")} onClick={()=>removeSsh(row.key)}><XIcon/></Button>}/>
        <SettingsRow id={`ssh-${row.key}-host`} label={t("account_editor.host")}><Input id={`ssh-${row.key}-host`} value={row.host} onChange={event=>editSsh(row.key,{host:event.target.value})}/></SettingsRow>
        <TextField id={`ssh-${row.key}-user`} label={t("account_editor.ssh_user")} value={row.user??""} placeholder={t("model_editor.default")} onChange={event=>editSsh(row.key,{user:event.target.value||null})}/>
        <TextField id={`ssh-${row.key}-port`} label={t("account_editor.ssh_port")} type="number" min={1} max={65535} value={row.ssh_port} onChange={event=>editSsh(row.key,{ssh_port:event.target.value})}/>
        <TextField id={`ssh-${row.key}-stage`} label={t("account_editor.dl_port")} type="number" min={1} max={65535} value={row.stage_port} onChange={event=>editSsh(row.key,{stage_port:event.target.value})}/>
      </SettingsGroup>)}
      {sshTargets(ssh)===null&&<p className="dl-settings-caption" role="status">{t("account_editor.incomplete_ssh")}</p>}
    </SettingsSection>}
  </div>;
}
