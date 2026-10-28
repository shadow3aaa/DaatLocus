import { t } from "@/lib/i18n";
import type { Device } from "@/lib/stage-api";
import { Badge } from "@/components/ui/badge";
import { LazyPage } from "@/components/lazy-page";
import { Spinner } from "@/components/ui/spinner";
import { StatusNote } from "./controls";
import { SettingsGroup, SettingsRow, SettingsSection } from "./structure";

const connectionKeys: Record<string, string> = {
  online: "account_editor.online",
  connecting: "account_editor.connecting",
  backoff: "account_editor.offline",
  unreachable: "account_editor.offline",
  disabled: "account_editor.offline",
};
const errorKeys: Record<string, string> = {
  device_relay_unavailable: "account_editor.device_relay_unavailable",
  device_connection_timeout: "account_editor.device_connection_timeout",
  device_connection_failed: "account_editor.device_connection_failed",
};

export function DeviceList({ local, devices, loading, error, cursor, onMore }: {
  local: { id: string; name: string } | null;
  devices: Device[];
  loading: boolean;
  error: string | null;
  cursor: string | null;
  onMore(signal: AbortSignal): Promise<unknown>;
}) {
  const peers = devices.filter(device => device.stage_id !== local?.id);
  return <SettingsSection title={t("account_editor.devices")}>
    <StatusNote text={error} error />
    {(local || peers.length > 0) && <SettingsGroup>
      {local && <SettingsRow label={local.name}><Badge variant="secondary">{t("account_editor.this_device")}</Badge></SettingsRow>}
      {peers.map(device => <SettingsRow key={device.stage_id} label={device.label || `${t("account_editor.unnamed_device")} (${device.stage_id.slice(0, 8)})`}>
        <div className="flex flex-col items-end gap-1">
        <Badge variant={device.connection.status === "online" && !error ? "secondary" : "outline"}>
          {t(error ? "account_editor.status_unknown" : connectionKeys[device.connection.status] || "account_editor.offline")}
        </Badge>
        {!error && device.connection.status !== "online" && device.error_code && device.error_code !== "device_peer_offline" && <StatusNote text={t(errorKeys[device.error_code] || "account_editor.device_connection_failed")} error />}
        </div>
      </SettingsRow>)}
    </SettingsGroup>}
    {loading && <Spinner />}
    {cursor && <LazyPage cursor={cursor} onLoad={onMore} errorMessage={t("lazy_page.load_failed")} />}
  </SettingsSection>;
}
