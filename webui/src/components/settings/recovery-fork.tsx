import { useEffect, useRef, useState } from "react";
import { t } from "@/lib/i18n";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Spinner } from "@/components/ui/spinner";
import { getRecoveryForkStatus, recoverFromBranch, type RecoveryForkRequest, type RecoveryForkStatus } from "@/lib/stage-api";
import { SettingsGroup, SettingsRow, SettingsSection } from "./structure";
import { errorText, Picker, StatusNote } from "./controls";

export function RecoveryFork() {
  const [open, setOpen] = useState(false);
  const [info, setInfo] = useState<RecoveryForkStatus | null>(null);
  const [source, setSource] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [retry, setRetry] = useState(0);
  const request = useRef<RecoveryForkRequest | null>(null);
  useEffect(() => {
    if (!open) return;
    const controller = new AbortController();
    setInfo(null); setError(null); request.current = null;
    void getRecoveryForkStatus({signal:controller.signal}).then(value => {
      if (!controller.signal.aborted) {setInfo(value);setSource(value.sources[0]?.source ?? "");}
    }).catch(cause => {if(!controller.signal.aborted)setError(errorText(cause));});
    return () => controller.abort();
  }, [open, retry]);
  async function recover() {
    if (!info || !info.eligible || pending) return;
    const selected = info.sources.find(item => item.source === source);
    if (!selected) return;
    request.current ??= {id:crypto.randomUUID(),account:info.account,target:info.target,source,
      source_branch:selected.branch,source_head:selected.head,expected_local_head:info.head,work_generation:info.work_generation};
    setPending(true); setError(null);
    try {await recoverFromBranch(request.current);window.location.assign("/");}
    catch(cause){setError(errorText(cause));setPending(false);}
  }
  return <SettingsSection title={t("recovery_fork.section")}><SettingsGroup>
    <SettingsRow label={t("recovery_fork.title")} description={t("recovery_fork.summary")}>
      <Button variant="outline" onClick={()=>setOpen(true)}>{t("recovery_fork.open")}</Button>
    </SettingsRow>
    <Dialog open={open} onOpenChange={next=>{if(!pending)setOpen(next);}}>
      <DialogContent showCloseButton={!pending}>
        <DialogHeader><DialogTitle>{t("recovery_fork.title")}</DialogTitle><DialogDescription>{t("recovery_fork.warning")}</DialogDescription></DialogHeader>
        <p>{t("recovery_fork.idle_source")}</p>
        {!info && !error && <Spinner/>}
        {info?.branch && <p role="status">{t("conversation_pane.local_recovery")}</p>}
        {info && !info.eligible && !info.branch && <p>{t("recovery_fork.not_fresh")}</p>}
        {info?.eligible && !info.sources.length && <p>{t("recovery_fork.no_source")}</p>}
        {info?.eligible && info.sources.length>0 && <Picker id="dl-recovery-source" label={t("recovery_fork.source")} value={source} disabled={pending} onChange={value=>{setSource(value);request.current=null;}} options={info.sources.map(item=>({value:item.source,label:item.label || item.source.slice(0,8)}))}/>}
        <StatusNote text={error} error/>
        <DialogFooter>
          <Button variant="outline" disabled={pending} onClick={()=>setRetry(value=>value+1)}>{t("recovery_fork.refresh")}</Button>
          <Button variant="outline" disabled={pending} onClick={()=>setOpen(false)}>{t("reset_work.cancel")}</Button>
          <Button disabled={pending || !info?.eligible || !source} onClick={()=>void recover()}>{pending&&<Spinner data-icon="inline-start"/>}{t("recovery_fork.confirm")}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  </SettingsGroup></SettingsSection>;
}
