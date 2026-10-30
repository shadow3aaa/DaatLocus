import { apiErrorText } from "./i18n";
import { subscribe, type StreamOptions } from "./event-stream";
import { serviceFetch } from "./transport";
export type { StreamStatus } from "./event-stream";
export type JsonValue = null | boolean | number | string | JsonValue[] | {
  [key: string]: JsonValue;
};
export type ActorStatus = "idle" | "running" | "archived";
export type MessageStatus = "queued" | "running" | "completed" | "failed" | "cancelled" | "interrupted";
export type ApprovalDecision = "application_required" | "pending" | "approved" | "denied" | "cancelled";
export type FetchOptions = {
  signal?: AbortSignal;
};
export type Page<T, C = number> = {
  items: T[];
  next_cursor: C | null;
  event_cursor?: number;
};

export type ActorConfig = {
  name: string;
  workspace: string;
  apps: string[];
  model: string | null;
  instructions: string;
};
export type ActorSummary = {
  id: string;
  parent_id?: string | null;
  conversation_url: string;
  name: string;
  status: ActorStatus;
  apps: string[];
  created_at_ms: number;
  updated_at_ms: number;
};
export type ActorDetail = {
  remote?: boolean;
  model_ready?: boolean;
  id: string;
  parent_id?: string | null;
  conversation_url: string;
  config?: ActorConfig;
  name?: string;
  status: ActorStatus;
  created_at_ms: number;
  updated_at_ms: number;
  access?: {
    view: boolean;
    converse: boolean;
    manage?: boolean;
  };
};
export const actorModelReady = (actor: ActorDetail | null, localReady: boolean): boolean =>
  actor?.remote ? actor.model_ready ?? true : localReady;
export type Access = {
  kind: "owner";
  manage: true;
  account?: {account_id:string;name:string;revision:number};
  session_id?: string | null;
};
export type Attachment = {
  id: string;
  name: string;
  media_type: string;
  size_bytes: number;
  uri: string;
};
export type MessageSource = {
  kind: "user";
} | {
  kind: "actor";
  actor_id: string;
} | {
  kind: "remote_user";
  stage_id: string;
  invocation_id: string;
} | {
  kind: "remote_actor";
  stage_id: string;
  actor_id: string;
  invocation_id: string;
};
export type ReplyTarget = {
  kind: "user";
} | {
  kind: "actor";
  actor_id: string;
  request_id: string;
} | {
  kind: "remote";
  stage_id: string;
  invocation_id: string;
  user: boolean;
};
export type MessageRecord = {
  seq: number;
  id: string;
  actor_id: string;
  source: MessageSource;
  reply_to: ReplyTarget;
  kind: "request" | "result" | "delivery" | "progress";
  in_reply_to: string | null;
  content: string;
  attachments: Attachment[];
  status: MessageStatus;
  cancel_requested: boolean;
  started_at_ms?: number | null;
  finished_at_ms?: number | null;
  final_generation?: number | null;
  reply: string | null;
  error: string | null;
  created_at_ms: number;
  updated_at_ms: number;
};
export type ToolSummary = {
  seq: number;
  id: string;
  message_id: string;
  call_id: string;
  name: string;
  summary: string;
  error: string | null;
  outcome?: JsonValue | null;
};
export type ToolResult = Omit<ToolSummary, "seq"> & {
  actor_id: string;
  arguments: JsonValue;
  payload: JsonValue;
  output_schema?: JsonValue;
  model_content: string;
};
export type ApprovalScope = {path:string;write:boolean;recursive:boolean};
export type ApprovalRecord = {
  scopes?: ApprovalScope[];
  previous_denials?: string[];
  seq: number;
  id: string;
  message_id: string;
  call: {
    id: string;
    name: string;
    arguments: JsonValue;
  };
  requested: {
    readable_roots: string[];
    writable_roots: string[];
  };
  decision: ApprovalDecision;
  reason: string;
  created_at_ms: number;
  resolved_at_ms: number | null;
};
export type ActorEvent = {
  type: "conversation_restored";
  archive_id: string;
} | {
  type: "actor_changed";
  actor: ActorSummary;
} | {
  type: "actor_deleted";
  actor_id: string;
} | {
  type: "message_added";
  message_id: string;
} | {
  type: "message_updated";
  message_id: string;
  status: MessageStatus;
} | {
  type: "generation_started";
  message_id: string;
  generation: number;
} | {
  type: "generation_delta";
  message_id: string;
  generation: number;
  channel: string;
  text: string;
} | {
  type: "tool_started";
  message_id: string;
  call_id: string;
  name: string;
  outcome?: JsonValue | null;
} | {
  type: "tool_finished";
  message_id: string;
  call_id: string;
  name: string;
  result_id: string;
  summary: string;
  error: string | null;
  outcome?: JsonValue | null;
} | {
  type: "approval_requested";
  approval_id: string;
  message_id: string;
  call_id: string;
  name: string;
} | {
  type: "approval_resolved";
  approval_id: string;
  message_id: string;
  decision: ApprovalDecision;
  reason: string;
} | {
  type: "attachment_added";
  attachment: Attachment;
};
export type EventRecord = {
  seq: number;
  actor_id: string;
  created_at_ms: number;
  event: ActorEvent;
};
export type ProviderKind = "openai" | "anthropic_compatible" | "openai_compatible" | "codex" | "open_code_console" | "open_code_go" | "github_copilot" | "ollama" | "ollama_cloud";
export type ProviderConfig = {
  kind: ProviderKind;
  name: string;
  api_key?: string | null;
  credential_configured?: boolean;
  credential_name?: string | null;
  base_url?: string | null;
  keep_alive?: string | null;
  codex_auth_method?: "api_key" | "browser_login" | "device_login" | "import_local_codex" | "import_auth_file" | "existing_auth_file" | null;
  codex_auth_file?: string | null;
  opencode_auth_method?: "device_login" | "existing_auth_file" | null;
  github_auth_method?: "device_login" | "manual_token" | "env_token" | null;
};
export type ModelConfig = {
  capabilities?: string | null;
  name: string;
  provider_name: string;
  model_id: string;
  context_window_tokens?: number | null;
  max_completion_tokens?: number | null;
  supports_vision?: boolean | null;
  thinking_budget?: string | null;
  temperature?: number | null;
  rpm?: number | null;
  request_timeout_secs?: number | null;
  stream_idle_timeout_secs?: number | null;
  auto_compact_token_limit?: number | null;
  effective_context_window_percent?: number | null;
  tool_output_max_tokens?: number | null;
  api_style?: string | null;
};
export type SetupConfigRequest = {
  locale?: string | null;
  providers: ProviderConfig[];
  models: ModelConfig[];
  main_model?: string | null;
  approval_model?: string | null;
  daemon_port?: number | null;
};
export type ConfigReadiness = {
  kind: "unconfigured" | "incomplete" | "complete";
  port: number;
  recovered: boolean;
  ready: boolean;
};
export type DiscoveredModel = {
  id: string;
  context_window_tokens?: number | null;
  max_completion_tokens?: number | null;
  supports_vision?: boolean | null;
  thinking_budgets: string[];
  reasoning_options?: ({type: "toggle"} | {type: "effort"; values: string[]} | {type: "budget_tokens"; min: number; max: number | null})[];
};
export type ProviderAuthStart = {
  flow_id: string;
  provider_kind: ProviderKind;
  verification_url: string;
  user_code: string;
  expires_at_ms: number;
  interval_secs: number;
};
export type ProviderAuthResponse = {
  api_key?: string | null;
  auth_file?: string | null;
  message: string;
};
const providerInput = ({ credential_configured: _configured, ...provider }: ProviderConfig) => provider;
const configInput = (config: SetupConfigRequest) => ({ ...config, providers: config.providers.map(providerInput) });
export class ActorApiError extends Error {
  constructor(message: string, public readonly status: number, public readonly code = "request_failed", public readonly retryAfterMs?: number) { super(message); this.name = "ActorApiError"; }
}
const segment = (value: string) => encodeURIComponent(value);
const actorPath = (id: string) => { const remote = /^([0-9a-f-]{36})~([a-zA-Z0-9_-]+)$/.exec(id); return remote ? `/v1/stages/${remote[1]}/actors/${remote[2]}` : `/v1/actors/${segment(id)}`; };
function query(path: string, values: Record<string, string | number | boolean | null | undefined>) {
  const params = new URLSearchParams();
  for (const [key, value] of Object.entries(values))
    if (value !== null && value !== undefined)
      params.set(key, String(value));
  return params.size ? `${path}?${params}` : path;
}
async function responseError(response: Response): Promise<ActorApiError> {
  const body = await response.json().catch(() => null) as {
    message?: string;
    code?: string;
  } | null;
  const retry = Number(response.headers.get("retry-after"));
  return new ActorApiError(apiErrorText(body?.code || "request_failed"), response.status, body?.code, retry > 0 && Number.isFinite(retry) ? retry * 1000 : undefined);
}
export async function request<T>(path: string, options: FetchOptions & {
  method?: string;
  body?: unknown;
  headers?: HeadersInit;
  raw?: BodyInit;
  empty?: boolean;
} = {}): Promise<T> {
  const headers = new Headers(options.headers);
  headers.set("Accept", "application/json");
  headers.set("X-Daat-Request", "1");
  if (options.body !== undefined)
    headers.set("Content-Type", "application/json");
  const response = await serviceFetch(path, { method: options.method || "GET", credentials: "same-origin", cache: "no-store", headers,
    signal: options.signal, body: options.raw ?? (options.body !== undefined ? JSON.stringify(options.body) : undefined) }).catch(error => { if (options.signal?.aborted) throw error; throw new ActorApiError(apiErrorText("connection_failed"), 0, "connection_failed"); });
  if (!response.ok)
    throw await responseError(response);
  return response.status === 204 || options.empty ? undefined as T : await response.json() as T;
}
export const getAccess = (options?: FetchOptions) => request<Access>("/v1/access", options);
export const renewSession = (options?: FetchOptions) => request<void>("/v1/account/session", { ...options, method: "POST" });
/** Only a confirmed expired/revoked session opens login; outages remain retryable. */
export async function restoreSession(options?: FetchOptions): Promise<Access | null> {
  try { await renewSession(options); return await getAccess(options); }
  catch (error) {
    if (error instanceof ActorApiError && error.status === 401) return null;
    throw error;
  }
}
export const accessKey = (access: Access) => access.session_id ? `owner:${access.session_id}` : "owner";
export const logoutSession = (options?: FetchOptions) => request<void>("/v1/account/session", { ...options, method: "DELETE" });
export const listActors = (options: FetchOptions & {
  after?: string;
  limit?: number;
} = {}) => request<Page<ActorSummary, string>>(query("/v1/actors", { after: options.after, limit: options.limit ?? 50 }), options);
export type TokenUsage = {
  input_tokens: number;
  cached_input_tokens: number;
  output_tokens: number;
  reasoning_output_tokens: number;
  total_tokens: number;
};
export type DailyTokenUsage = {
  date: string;
  usage: TokenUsage;
};
export type TokenUsageInfo = {
  total_token_usage: TokenUsage;
  last_token_usage: TokenUsage;
  model_context_window: number | null;
  daily_token_usage: DailyTokenUsage[];
};
export const getActorUsage = (id: string, options?: FetchOptions) => request<TokenUsageInfo>(`${actorPath(id)}/usage`, options);
export const getActor = (id: string, options?: FetchOptions) => request<ActorDetail>(actorPath(id), options);
export const patchActorPreferences = (id: string, preferences: {
  model?: string | null;
  // Account-wide; accepted only for Core.
}, options?: FetchOptions) => request<ActorDetail>(actorPath(id), { ...options, method: "PATCH", body: preferences });
export type ConversationPage = Page<MessageRecord> & { active?: MessageRecord | null };
export const listMessages = (actor: string, options: FetchOptions & {
  before?: number;
  limit?: number;
} = {}) => request<ConversationPage>(query(`${actorPath(actor)}/messages`, { before: options.before, limit: options.limit ?? 30 }), options);
export const getMessage = (actor: string, id: string, options?: FetchOptions) => request<MessageRecord>(`${actorPath(actor)}/messages/${segment(id)}`, options);
export const sendCoreMessage = (input: {
  content: string;
  attachments: string[];
}, idempotencyKey: string, options?: FetchOptions) => request<MessageRecord>("/v1/core/messages", { ...options, method: "POST", body: input, headers: { "Idempotency-Key": idempotencyKey } });
export const listMessageActivity = (actor: string, message: string, options: FetchOptions & { after?: number; limit?: number } = {}) => request<Page<EventRecord>>(query(`${actorPath(actor)}/messages/${segment(message)}/activity`, { after: options.after, limit: options.limit ?? 100 }), options);
export const validateAttachment = async (actor: string, item: Attachment, options?: FetchOptions) => {
  const uri = resourceUrl(actor, item.id);
  const response = await serviceFetch(uri, { method: "HEAD", credentials: "same-origin", cache: "no-store", signal: options?.signal });
  if ([403,404,410].includes(response.status)) return false;
  if (!response.ok) throw await responseError(response);
  return true;
};
export const searchMessages = (actor: string, q: string, options: FetchOptions & {
  before?: number;
  limit?: number;
} = {}) => request<Page<MessageRecord>>(query(`${actorPath(actor)}/messages/search`, { q, before: options.before, limit: options.limit ?? 30 }), options);
export const listToolSummaries = (actor: string, message: string, options: FetchOptions & {
  before?: number;
  limit?: number;
} = {}) => request<Page<ToolSummary>>(query(`${actorPath(actor)}/messages/${segment(message)}/results`, { before: options.before, limit: options.limit ?? 30 }), options);
export const getToolResult = (actor: string, id: string, options?: FetchOptions) => request<ToolResult>(`${actorPath(actor)}/results/${segment(id)}`, options);
export const listApprovals = (actor?: string, options: FetchOptions & {
  before?: number;
  limit?: number;
} = {}) => request<Page<ApprovalRecord>>(query(actor ? `${actorPath(actor)}/approvals` : "/v1/approvals", { before: options.before, limit: options.limit ?? 30 }), options);
export const getApproval = (id: string, actor?: string, options?: FetchOptions) => request<ApprovalRecord>(`${actor ? `${actorPath(actor)}/approvals` : "/v1/approvals"}/${segment(id)}`, options);
export const resourceUrl = (actor: string, resource: string) => `${actorPath(actor)}/resources/${segment(resource)}`;
export const uploadAttachment = (file: File, options?: FetchOptions) => {
  const maximum = file.type.startsWith("image/") ? 10 * 1024 * 1024 : 32 * 1024 * 1024;
  if (!file.size || file.size > maximum)
    return Promise.reject(new ActorApiError(apiErrorText("payload_too_large"), 413, "payload_too_large"));
  return request<Attachment>("/v1/core/attachments", { ...options, method: "POST", raw: file, headers: { "X-File-Name": encodeURIComponent(file.name), "X-File-Name-Encoding": "utf-8-percent", "Content-Type": file.type || "application/octet-stream" } });
};
export const getConfig = (options?: FetchOptions) => request<SetupConfigRequest>("/v1/config", options);
export const getConfigReadiness = (options?: FetchOptions) => request<ConfigReadiness>("/v1/config/readiness", options);
export const saveConfig = (config: SetupConfigRequest, options?: FetchOptions) => request<ConfigReadiness>("/v1/config", { ...options, method: "PUT", body: configInput(config) });
export const previewConfig = (config: SetupConfigRequest, options?: FetchOptions) => request<ConfigReadiness>("/v1/config/preview", { ...options, method: "POST", body: configInput(config) });
export const discoverModels = (provider: ProviderConfig, options?: FetchOptions) => request<{
  models: DiscoveredModel[];
  source: "provider" | "catalog";
}>("/v1/config/models/discover", { ...options, method: "POST", body: { provider: providerInput(provider) } });
export const runProviderAuth = (provider: ProviderConfig, options?: FetchOptions) => request<ProviderAuthResponse>("/v1/config/auth/run", { ...options, method: "POST", body: { provider: providerInput(provider) } });
export const startProviderAuth = (provider: ProviderConfig, options?: FetchOptions) => request<ProviderAuthStart>("/v1/config/auth/start", { ...options, method: "POST", body: { provider: providerInput(provider) } });
export const completeProviderAuth = (provider: ProviderConfig, flowId: string, options?: FetchOptions) => request<ProviderAuthResponse>("/v1/config/auth/complete", { ...options, method: "POST", body: { provider: providerInput(provider), flow_id: flowId } });
export const subscribeActorEvents = (actor: string, options: StreamOptions) => subscribe(`${actorPath(actor)}/event-stream`, options);
export const subscribeCatalogEvents = (options: StreamOptions) => subscribe("/v1/event-stream", options);
