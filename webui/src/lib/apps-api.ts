import { request, type FetchOptions, type Page } from "./actor-api";

export type AppTimeouts = {
  connect_timeout_ms: number;
  startup_timeout_ms: number | null;
  request_timeout_ms: number | null;
};
export const defaultAppTimeouts: AppTimeouts = {
  connect_timeout_ms: 30000,
  startup_timeout_ms: null,
  request_timeout_ms: null,
};
export type AppSource =
  | {
      transport: "http";
      url: string;
      credential_configured: boolean;
      timeouts?: AppTimeouts;
    }
  | { transport: "stdio"; command: string; timeouts?: AppTimeouts };
export type InstalledApp = {
  id: string;
  name: string;
  description?: string;
  revision: number;
  enabled: boolean;
  source: AppSource;
  tool_count: number;
};
export type AppDetail = InstalledApp & {
  tools: { name: string; description: string }[];
};
export type InstallApp = {
  request_id?: string;
  name: string;
  description?: string;
  source:
    | {
        transport: "http";
        url: string;
        bearer_token: string | null;
        timeouts?: AppTimeouts;
      }
    | {
        transport: "stdio";
        command: string;
        arguments: string[];
        environment: Record<string, string>;
        timeouts?: AppTimeouts;
      };
};
export const listApps = (
  options: FetchOptions & {
    after?: string;
    search?: string;
    enabled?: boolean;
  } = {},
) =>
  request<
    Page<InstalledApp, string> & { revision: number; stdio_supported: boolean }
  >(
    `/v1/apps?limit=24${options.after ? `&after=${encodeURIComponent(options.after)}` : ""}${options.search ? `&search=${encodeURIComponent(options.search)}` : ""}${options.enabled === undefined ? "" : `&enabled=${options.enabled}`}`,
    options,
  );
export const getApp = (id: string, options?: FetchOptions) =>
  request<AppDetail>(`/v1/apps/${encodeURIComponent(id)}`, options);
export const installApp = (input: InstallApp, options?: FetchOptions) =>
  request<InstalledApp>("/v1/apps", {
    ...options,
    method: "POST",
    body: input,
  });
export const setAppEnabled = (
  app: InstalledApp,
  enabled: boolean,
  options?: FetchOptions,
) =>
  request<InstalledApp>(`/v1/apps/${encodeURIComponent(app.id)}/state`, {
    ...options,
    method: "POST",
    body: { expected_revision: app.revision, enabled },
  });

export const setAppTimeouts = (app: InstalledApp, timeouts: AppTimeouts) =>
  request<InstalledApp>(`/v1/apps/${encodeURIComponent(app.id)}/timeouts`, {
    method: "POST",
    body: { expected_revision: app.revision, timeouts },
  });
export const cancelAppInstallation = (id: string) =>
  request(`/v1/apps/installations/${encodeURIComponent(id)}`, {
    method: "DELETE",
  });
