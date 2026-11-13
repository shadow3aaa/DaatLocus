import { request, type FetchOptions, type Page } from "./actor-api";
export type DeviceProfile = { stage_id: string; public_key: string; tls_sha256: string };
export type WorkGeneration = {revision: number; writer: string};
export type ResetStatus = {account_id: string; name: string; generation: WorkGeneration | null; resetting: boolean};
export const getResetStatus = (options?: FetchOptions) => request<ResetStatus>("/v1/account/reset", options);
export const resetAccountWork = (status: ResetStatus) => request<void>("/v1/account/reset", {method: "POST", body: {account_id: status.account_id, expected_generation: status.generation}});
export type Connectivity = "local" | "relay";
export type DeviceSettings = { label: string; mode: Connectivity; relay_origin: string | null; compute_priority: number };
export type DevicePreferences = Partial<Pick<DeviceSettings,"label"|"relay_origin"|"mode">>;
export type Device = { stage_id: string; label: string | null; error_code?: string | null; connection: { status: string; route: string | null; generation: number; retry_at_ms: number } };
export type StageInfo = { stage_id: string; account_id: string | null; name: string | null; settings: DeviceSettings; public_login: PublicLoginStatus };
export const getStage = (options?: FetchOptions) => request<StageInfo>("/v1/stage", options);
export const getDevices = (options: FetchOptions & { after?: string } = {}) => request<Page<Device,string>>(
  `/v1/stage/devices?limit=64${options.after ? `&after=${encodeURIComponent(options.after)}` : ""}`, options,
);
export const saveDeviceSettings = (preferences: DevicePreferences) => request<DeviceSettings>("/v1/stage/settings", {method:"PATCH",body:preferences});

export type StageStatus = { public_login: PublicLoginStatus; work_generation?: WorkGeneration | null; model_ready: boolean };
export const getStageStatus = (options?: FetchOptions) => request<StageStatus>("/v1/stage/status", options);
export type ConversationConnectionMessage = "conversation_pane.reconnecting";

export type PublicLoginStatus = { state: "not_configured" | "waiting_for_account" | "connecting" | "online" | "reconnecting"; error_code: string | null; changed_at_ms: number; last_confirmed_at_ms: number | null; retry_at_ms: number | null };
