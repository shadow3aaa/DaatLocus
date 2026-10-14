import { invoke, isTauri } from "@tauri-apps/api/core";
import { ActorApiError, request } from "./actor-api";
import { apiErrorText } from "./i18n";

type Reply<T> = { status: number; body: T & { code?: string } };
type Info = { registered: boolean };
async function exchange<T>(input: Record<string, unknown>): Promise<T> {
  const reply = await invoke<Reply<T>>("desktop_account", { input }).catch(() => {
    throw new ActorApiError(apiErrorText("connection_failed"), 0, "connection_failed");
  });
  if (reply.status < 200 || reply.status >= 300)
    throw new ActorApiError(apiErrorText(reply.body?.code || "request_failed"), reply.status, reply.body?.code);
  return reply.status === 204 ? undefined as T : reply.body;
}

/** The native bridge can bootstrap an unregistered device; sessions stay in HTTP cookies. */
export async function accountRequest<T>(path: string, method = "GET", body?: Record<string, unknown>): Promise<T> {
  if (!isTauri()) return request<T>(path, { method, body });
  if (path === "/v1/account") return exchange<T>({ kind: "info" });
  const operation: Record<string, string> = {
    "/v1/account/registration/start": "register_start",
    "/v1/account/registration/finish": "register_finish",
    "/v1/account/login/start": "login_start",
    "/v1/account/login/import": "login_import",
    "/v1/account/connection": "connection",
  };
  const kind = operation[path];
  if (kind && !(await exchange<Info>({ kind: "info" })).registered)
    return exchange<T>({ ...body, kind });
  return request<T>(path, { method, body });
}
