import { request, type FetchOptions, type Page } from "./actor-api";

export type AppSource =
  | { transport: "http"; url: string; credential_configured: boolean }
  | { transport: "stdio"; command: string };
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
  name: string;
  description?: string;
  source:
    | { transport: "http"; url: string; bearer_token: string | null }
    | {
        transport: "stdio";
        command: string;
        arguments: string[];
        environment: Record<string, string>;
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
