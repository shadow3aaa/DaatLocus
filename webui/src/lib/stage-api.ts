import { request, type FetchOptions, type Page } from "./actor-api";
export type DeviceProfile = { stage_id: string; public_key: string; tls_sha256: string };
export type WorkGeneration = {revision: number; writer: string};
export type ResetStatus = {account_id: string; name: string; generation: WorkGeneration | null; resetting: boolean};
export const getResetStatus = (options?: FetchOptions) => request<ResetStatus>("/v1/account/reset", options);
export const resetAccountWork = (status: ResetStatus) => request<void>("/v1/account/reset", {method: "POST", body: {account_id: status.account_id, expected_generation: status.generation}});
export type SshTarget = { host: string; user: string | null; ssh_port: number; stage_port: number };
export type DeviceSettings = { label: string; listen: string; interfaces: string[]; signaling_origin: string | null; stun: string[]; compute_priority: number; ssh: Record<string,SshTarget> };
export type DevicePreferences = Partial<Pick<DeviceSettings,"label"|"signaling_origin"|"ssh">>;
export type Device = { stage_id: string; label: string | null; error_code?: string | null; connection: { status: string; route: string | null; generation: number; retry_at_ms: number } };
export type StageInfo = { stage_id: string; account_id: string | null; name: string | null; settings: DeviceSettings; address: string | null; primary_actor_id: string; core: CoreStatus | null; public_login: PublicLoginStatus };
export const getStage = (options?: FetchOptions) => request<StageInfo>("/v1/stage", options);
export const getDevices = (options: FetchOptions & { after?: string } = {}) => request<Page<Device,string>>(
  `/v1/stage/devices?limit=64${options.after ? `&after=${encodeURIComponent(options.after)}` : ""}`, options,
);
export const saveDeviceSettings = (preferences: DevicePreferences) => request<DeviceSettings>("/v1/stage/settings", {method:"PATCH",body:preferences});

export type CoreStatus = { worker_stage_id: string | null; epoch: number; synchronized: boolean; ready: boolean; model_ready: boolean; state: string; error_code?: string | null; config_revision?: string | null };
export const getStageStatus = (options?: FetchOptions) => request<{core: CoreStatus | null; public_login: PublicLoginStatus; work_generation?: WorkGeneration | null}>("/v1/stage/status", options);
export type ConversationConnectionMessage = "conversation_pane.synchronizing" | "conversation_pane.reconnecting" | "conversation_pane.recovering" | "conversation_pane.reconciling" | "conversation_pane.offline" | "conversation_pane.recovery_blocked" | "app.connecting";
export function connectionBlocks(message: ConversationConnectionMessage | null | undefined): boolean { return Boolean(message); }
export function coreConnectionMessage(core: CoreStatus | null | undefined): ConversationConnectionMessage | null {
  if (core?.ready) return null;
  if (core?.state === "recovering") return "conversation_pane.recovering";
  if (core?.state === "reconciling") return "conversation_pane.reconciling";
  if (core?.state === "offline") return "conversation_pane.offline";
  if (core?.state === "recovery_blocked") return "conversation_pane.recovery_blocked";
  if (core?.state === "model_required") return null;
  return core?.state === "synchronizing" ? "conversation_pane.synchronizing" : "app.connecting";
}

export type PublicLoginStatus = { state: "not_configured" | "waiting_for_account" | "connecting" | "online" | "reconnecting"; error_code: string | null; changed_at_ms: number; last_confirmed_at_ms: number | null; retry_at_ms: number | null };
