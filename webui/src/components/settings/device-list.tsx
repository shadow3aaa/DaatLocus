import { t } from "@/lib/i18n";
import type { Device } from "@/lib/stage-api";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
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
  device_signaling_unavailable: "account_editor.device_signaling_unavailable",
  device_connection_timeout: "account_editor.device_connection_timeout",
  device_peer_offline: "account_editor.device_peer_offline",
  device_connection_failed: "account_editor.device_connection_failed",
};

export function DeviceList({ local, devices, loading, error, hasMore, onMore }: {
  local: { id: string; name: string } | null;
  devices: Device[];
  loading: boolean;
  error: string | null;
  hasMore: boolean;
  onMore(): void;
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
        {!error && device.connection.status !== "online" && device.error_code && <StatusNote text={t(errorKeys[device.error_code] || "account_editor.device_connection_failed")} error />}
        </div>
      </SettingsRow>)}
    </SettingsGroup>}
    {loading && <Spinner />}
    {hasMore && <Button variant="ghost" size="sm" disabled={loading} onClick={onMore}>{t("account_editor.more_devices")}</Button>}
  </SettingsSection>;
}
