import { appsRoute } from "./apps";
import { configureComputerUse, getComputerUseStatus } from "./computer-use";
import { importsRoute } from "./imports";
import { advanceActivityPreview } from "./activity";
/**
 * Development fixtures for the client HTTP/SSE wire shapes. Authentication,
 * permissions and actual execution remain the real service's responsibility.
 */
import type {
  Access,
  ApprovalMode,
  ApprovalRecord,
  Attachment,
  ConfigReadiness,
  MessageRecord,
  Page,
  SetupConfigRequest,
} from "../src/lib/actor-api";
import type { CoreStatus, Device, DeviceSettings, PublicLoginStatus, StageInfo } from "../src/lib/stage-api";
import { cancelTurn, isActorBusy, submitTurn } from "./engine";
import { activateScenario, previewState, scenarios } from "./scenarios";
import { sseResponse } from "./sse";
import { resetAccountWork } from "./fixtures";
import {
  ACCOUNT_ID,
  ACCOUNT_NAME,
  CATALOG_KEY,
  MOCK_PORT,
  ROOT_ACTOR_ID,
  STAGE_ID,
  addMessage,
  configHolder,
  emitEvent,
  findApproval,
  getActor,
  maxSeq,
  newId,
  now,
  resolveApprovalRecord,
  stageSettings,
  store,
  toolSummary,
  touchMessage,
  workReset,
  type ActorStore,
} from "./state";

/** Mirrors AccountInfo in src/lib/account-api.ts (that module pulls WASM imports, so it is not type-imported). */
export type AccountInfo = {
  registered: boolean;
  account_id: string | null;
  name: string | null;
  can_create: boolean;
  relay_origin: string | null;
};

const ACCESS: Access = {
  kind: "owner",
  manage: true,
  account: { account_id: ACCOUNT_ID, name: ACCOUNT_NAME, revision: 1 },
  session_id: "mock-session",
};

function json(data: unknown, status = 200): Response {
  return new Response(JSON.stringify(data), { status, headers: { "Content-Type": "application/json" } });
}
function notFound(message = "Resource not found"): Response {
  return json({ message, code: "not_found" }, 404);
}
/** Backend maps validation failures to 422 invalid_request, never 400. */
function invalidRequest(message = "Request is invalid"): Response {
  return json({ message, code: "invalid_request" }, 422);
}
function conflict(message = "Operation conflicts with current state"): Response {
  return json({ message, code: "conflict" }, 409);
}
function noContent(): Response {
  return new Response(null, { status: 204 });
}

/**
 * Backend contract (src/actors/store.rs): the query fetches newest-first with
 * one extra row, then reverses — items arrive ASCENDING by seq and
 * next_cursor is the oldest seq of the page when more remain.
 */
function pageDesc<T extends { seq: number }>(items: T[], before: number | null, limit: number): Page<T, number> {
  const filtered = items.filter(item => before === null || item.seq < before).sort((a, b) => b.seq - a.seq);
  const page = filtered.slice(0, limit);
  const next = filtered.length > page.length && page.length ? page[page.length - 1].seq : null;
  return { items: page.reverse(), next_cursor: next };
}

function pageAsc<T extends { seq: number }>(items: T[], after: number | null, limit: number): Page<T, number> {
  const filtered = items.filter(item => after === null || item.seq > after).sort((a, b) => a.seq - b.seq);
  const page = filtered.slice(0, limit);
  return { items: page, next_cursor: filtered.length > page.length && page.length ? page[page.length - 1].seq : null };
}

function intParam(url: URL, name: string): number | null {
  const raw = url.searchParams.get(name);
  if (raw === null || raw === "") return null;
  const value = Number.parseInt(raw, 10);
  return Number.isFinite(value) ? value : null;
}

/** Returns null when the parameter is present but out of range (backend: 422). */
function limitParam(url: URL, name: string, fallback: number, max = 100): number | null {
  const raw = url.searchParams.get(name);
  if (raw === null || raw === "") return fallback;
  const value = Number.parseInt(raw, 10);
  return Number.isFinite(value) && value >= 1 && value <= max ? value : null;
}

async function jsonBody(req: Request): Promise<Record<string, unknown> | null> {
  try {
    return await req.json() as Record<string, unknown>;
  } catch {
    return null;
  }
}

function coreStatus(): CoreStatus {
  return {...previewState.core};
}

function publicLogin(): PublicLoginStatus {
  return { state: "not_configured", error_code: null, changed_at_ms: 0, last_confirmed_at_ms: null, retry_at_ms: null };
}

function readiness(): ConfigReadiness {
  const ready=configHolder.current.models.length>0;
  return { kind: ready?"complete":"unconfigured", port: MOCK_PORT, recovered: false, ready };
}

function decodeSegment(value: string): string {
  try {
    return decodeURIComponent(value);
  } catch {
    return value;
  }
}

function findMessage(actor: ActorStore, id: string): MessageRecord | undefined {
  return actor.messages.find(message => message.id === id);
}

function attachmentsFromIds(ids: unknown): Attachment[] {
  if (!Array.isArray(ids)) return [];
  const attachments: Attachment[] = [];
  for (const id of ids)
    if (typeof id === "string") {
      const blob = store.attachments.get(id);
      if (blob) attachments.push(blob.attachment);
    }
  return attachments;
}

const RE_ACTOR = /^\/v1\/actors\/([^/]+)$/;
const RE_MESSAGES = /^\/v1\/actors\/([^/]+)\/messages$/;
const RE_MESSAGE_SEARCH = /^\/v1\/actors\/([^/]+)\/messages\/search$/;
const RE_MESSAGE = /^\/v1\/actors\/([^/]+)\/messages\/([^/]+)$/;
const RE_MESSAGE_ACTION = /^\/v1\/actors\/([^/]+)\/messages\/([^/]+)\/(cancel|edit|withdraw|steer)$/;
const RE_ACTIVITY = /^\/v1\/actors\/([^/]+)\/messages\/([^/]+)\/activity$/;
const RE_RESULTS = /^\/v1\/actors\/([^/]+)\/messages\/([^/]+)\/results$/;
const RE_RESULT = /^\/v1\/actors\/([^/]+)\/results\/([^/]+)$/;
const RE_QUEUE = /^\/v1\/actors\/([^/]+)\/queue$/;
const RE_QUEUE_CLEAR = /^\/v1\/actors\/([^/]+)\/queue\/clear$/;
const RE_ACTOR_APPROVALS = /^\/v1\/actors\/([^/]+)\/approvals$/;
const RE_ACTOR_APPROVAL_ITEM = /^\/v1\/actors\/([^/]+)\/approvals\/([^/]+)$/;
const RE_APPROVAL = /^\/v1\/approvals\/([^/]+)$/;
const RE_ATTACHMENTS = /^\/v1\/actors\/([^/]+)\/attachments$/;
const RE_RESOURCE = /^\/v1\/actors\/([^/]+)\/resources\/([^/]+)$/;
const RE_ACTOR_STREAM = /^\/v1\/actors\/([^/]+)\/event-stream$/;
const RE_REMOTE = /^\/v1\/stages\//;

/** The real service adds these to every response (src/service/mod.rs). */
function contract(response: Response): Response {
  response.headers.set("Cache-Control", "private, no-store");
  response.headers.set("X-Content-Type-Options", "nosniff");
  return response;
}

export async function handle(req: Request): Promise<Response> {
  const url = new URL(req.url);
  const path = url.pathname.length > 1 ? url.pathname.replace(/\/+$/, "") : url.pathname;
  const method = req.method.toUpperCase();
  if (method === "OPTIONS") return contract(noContent());
  try {
    const response = await route(method, path, url, req);
    return contract(response ?? notFound(`No mock route for ${method} ${path}`));
  } catch {
    return contract(json({ message: "Internal server error", code: "internal" }, 500));
  }
}

async function route(method: string, path: string, url: URL, req: Request): Promise<Response | null> {
  if (path === "/v1/device/virtual-displays/capabilities" && method === "GET") return json({ supported: process.env.DAAT_LOCUS_MOCK_MACOS !== "1" });
  if (path === "/v1/device/virtual-displays" && method === "GET") return json({ supported: true, state: getComputerUseStatus().enabled ? "ready" : "disabled", displays: [] });
  if (path === "/v1/device/computer-use/capabilities" && method === "GET") return json({supported: true});
  if (path === "/v1/device/computer-use") {
    if (method === "GET") return json(getComputerUseStatus());
    if (method === "PATCH") return json(await configureComputerUse(await req.json()));
  }
  let match: RegExpMatchArray | null;

  if (RE_MESSAGES.test(path) && method === "GET") {
    if(previewState.id==="loading") await new Promise(resolve=>setTimeout(resolve,2000));
    if(previewState.id==="request-error") return json({message:"Service unavailable",code:"unavailable"},503);
  }
  if(previewState.id==="reconnecting" && (path==="/v1/event-stream"||RE_ACTOR_STREAM.test(path))) return json({message:"Stream unavailable",code:"unavailable"},503);

  // ---- mock control -------------------------------------------------------
  if (path === "/__mock/activity/step" && method === "POST") {
    return ["activity-replay", "activity-overflow"].includes(previewState.id) && advanceActivityPreview() ? noContent() : conflict("No further activity phase");
  }
  if(path==="/__mock/frame" && method==="GET") {
    const id=url.searchParams.get("scene")??"conversation";
    const scene=scenarios.find(item=>item.id===id);
    if(!scene) return invalidRequest("Unknown preview scenario");
    const requestedWidth=Number(url.searchParams.get("width")??390);
    const width=Number.isInteger(requestedWidth)&&requestedWidth>=320&&requestedWidth<=1440?requestedWidth:390;
    return new Response(`<!doctype html><html lang="zh-CN"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>${scene.label} · ${width}px</title><style>body{margin:0;height:100dvh;display:flex;flex-direction:column;background:#e9e9e9;font:14px/1.5 system-ui,sans-serif}nav{display:flex;flex-wrap:wrap;justify-content:center;align-items:center;gap:16px;padding:8px 12px;min-height:45px;box-sizing:border-box}a{color:#333}label{display:inline-flex;align-items:center;gap:6px}input{width:76px;font:inherit;padding:2px 6px}iframe{display:block;flex:1;min-height:0;border:0;margin:0 auto;width:${width}px;max-height:844px;max-width:100%;background:#fcfcfc}</style><nav><a href="/__mock">所有场景</a><a href="/__mock/frame?scene=${scene.id}&width=320">320px</a><a href="/__mock/frame?scene=${scene.id}&width=390">390px</a>${["activity-replay","activity-overflow"].includes(scene.id)?'<button id="activity-step" type="button">下一阶段</button><a href="/__mock/frame?scene='+scene.id+'&width='+width+'">重新开始</a>':""}<label>预览宽度 <input id="preview-width" type="number" min="320" max="1440" value="${width}">px</label></nav><iframe id="preview-frame" title="${scene.label}" src="/__mock/open?scene=${scene.id}"></iframe><script>const widthInput=document.getElementById('preview-width');widthInput.addEventListener('input',()=>{const next=widthInput.valueAsNumber;if(Number.isInteger(next)&&next>=320&&next<=1440)document.getElementById('preview-frame').style.width=next+'px';});const step=document.getElementById('activity-step');let phase=0;if(step)step.addEventListener('click',async()=>{step.disabled=true;const response=await fetch('/__mock/activity/step',{method:'POST'});if(response.ok){phase++;step.textContent=phase===4?'演示已完成':'下一阶段（'+phase+'/4）';step.disabled=phase===4;}else{step.textContent='请重新开始';}});</script></html>`,{headers:{"Content-Type":"text/html; charset=utf-8"}});
  }
  if(path==="/__mock" && method==="GET") return new Response(`<!doctype html><html lang="zh-CN"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Daat Locus · UI 预览</title><style>body{font:15px/1.5 system-ui,sans-serif;margin:0;background:#fafafa;color:#222}main{max-width:800px;margin:48px auto;padding:0 24px}h1{font-size:24px;margin-bottom:8px}p{color:#666}nav{display:grid;grid-template-columns:repeat(auto-fit,minmax(220px,1fr));gap:12px;margin-top:28px}article{padding:16px;border:1px solid #ddd;border-radius:12px;background:white}article>a{display:block}article>span{display:flex;gap:16px;margin-top:12px}article>span a{font-size:13px;color:#666}a{color:inherit;text-decoration:none;padding:0;border:1px solid #ddd;border-radius:12px;background:white}a:hover{border-color:#999}small{display:block;margin-top:6px;color:#777}</style><main><h1>WebUI 场景预览</h1><p>使用实际 WebUI 组件和内存数据。切换场景会清除本次预览的草稿以外的数据；不会连接真实账号或设备。</p><nav>${scenarios.map(scene=>`<article><a href="/__mock/${["activity-replay","activity-overflow"].includes(scene.id)?"frame":"open"}?scene=${scene.id}${["activity-replay","activity-overflow"].includes(scene.id)?"&width=1000":""}">${scene.label}<small>${scene.id}</small></a><span><a href="/__mock/frame?scene=${scene.id}&width=390">手机 390px</a><a href="/__mock/frame?scene=${scene.id}&width=320">窄屏 320px</a></span></article>`).join("")}</nav></main></html>`,{headers:{"Content-Type":"text/html; charset=utf-8"}});
  if(path==="/__mock/scenarios" && method==="GET") return json({items:scenarios,current:previewState.id});
  if(path==="/__mock/open" && method==="GET") {
    const id=url.searchParams.get("scene")??"conversation";
    if(!activateScenario(id)) return invalidRequest("Unknown preview scenario");
    const scene=scenarios.find(item=>item.id===id)!;
    return new Response(null,{status:303,headers:{Location:scene.id==="imports"?"/settings/import":scene.id.startsWith("apps")?"/apps":scene.actor===ROOT_ACTOR_ID?"/":`/actors/${scene.actor}`}});
  }
  if(path==="/__mock/scenario" && method==="POST") {const body=await jsonBody(req);if(typeof body?.id!=="string"||!activateScenario(body.id))return invalidRequest("Unknown preview scenario");return json({id:previewState.id});}
  if (path === "/__mock/health" && method === "GET") return json({ ok: true });
  if (path === "/__mock/reset" && method === "POST") {
    activateScenario("conversation");
    return json({ ok: true, reset: true });
  }

  if (path.startsWith("/v1/apps")) {
    if (!previewState.authenticated) return json({code:"unauthorized"},401);
    if (method === "GET" && previewState.id === "apps-loading") await new Promise(resolve => setTimeout(resolve, 2000));
    if (method === "GET" && previewState.id === "apps-error") return json({code:"unavailable"},503);
    return appsRoute(method, path, url, req);
  }

  // ---- account / access ---------------------------------------------------
  if (path === "/v1/access" && method === "GET") return previewState.authenticated?json(ACCESS):json({message:"Sign in required",code:"unauthorized"},401);
  if (path === "/v1/account" && method === "GET") {
    const account: AccountInfo = {
      registered: previewState.registered,
      account_id: ACCOUNT_ID,
      name: ACCOUNT_NAME,
      can_create: true,
      relay_origin: null,
    };
    return json(account);
  }
  if (path === "/v1/account/connection" && method === "PUT") return noContent();
  if (path === "/v1/account/reset") {
    if (!previewState.authenticated) return json({code: "unauthorized", message: "Sign in required"}, 401);
    if (method === "GET") return json({account_id: ACCOUNT_ID, name: ACCOUNT_NAME, generation: workReset.generation, resetting: false});
    if (method === "POST") {
      const body = await jsonBody(req);
      if (body?.account_id !== ACCOUNT_ID) return invalidRequest();
      if (JSON.stringify(body?.expected_generation) !== JSON.stringify(workReset.generation)) return json({code: "conflict", message: "Account work changed"}, 409);
      resetAccountWork();
      return noContent();
    }
  }
  if (path === "/v1/account/session" && method === "DELETE") return noContent();

  // ---- stage --------------------------------------------------------------
  if (path === "/v1/imports" && method === "GET") return json({codex: previewState.id !== "imports-unsupported"});
  if (path.startsWith("/v1/imports/") && ["POST","GET"].includes(method)) {
    if(previewState.id==="imports-unsupported")return notFound();
    if (!previewState.authenticated) return json({code: "unauthorized"}, 401);
    return importsRoute(path, req);
  }
  if (path === "/v1/stage" && method === "GET") {
    const stage: StageInfo = {
      stage_id: STAGE_ID,
      account_id: ACCOUNT_ID,
      name: ACCOUNT_NAME,
      settings: { ...stageSettings },
      primary_actor_id: ROOT_ACTOR_ID,
      core: coreStatus(),
      public_login: publicLogin(),
    };
    return json(stage);
  }
  if (path === "/v1/stage/status" && method === "GET") return json({ core: coreStatus(), public_login: publicLogin(), work_generation: workReset.generation });
  if (path === "/v1/stage/devices" && method === "GET") {
    const limit = limitParam(url, "limit", 64, 64);
    if (limit === null) return invalidRequest();
    const device: Device = {
      stage_id: STAGE_ID,
      label: stageSettings.label,
      error_code: null,
      connection: { status: "online", route: null, generation: 1, retry_at_ms: 0 },
    };
    return json({ items: [device,...previewState.devices], next_cursor: null } satisfies Page<Device, string>);
  }
  if (path === "/v1/stage/settings" && method === "PATCH") {
    const body = await jsonBody(req);
    if (!body) return invalidRequest("Expected a JSON body");
    const merged: DeviceSettings = { ...stageSettings };
    if (typeof body.label === "string") merged.label = body.label;
    if (body.relay_origin === null || typeof body.relay_origin === "string") merged.relay_origin = body.relay_origin;
    if (body.mode === "local" || body.mode === "relay") merged.mode = body.mode;
    Object.assign(stageSettings, merged);
    return json({ ...stageSettings });
  }

  // ---- SSE streams --------------------------------------------------------
  if (path === "/v1/event-stream" && method === "GET") return sseResponse(CATALOG_KEY, req.headers.get("last-event-id"));
  if ((match = RE_ACTOR_STREAM.exec(path)) && method === "GET") {
    const actor = getActor(decodeSegment(match[1]));
    if (!actor) return notFound("Unknown actor");
    return sseResponse(actor.summary.id, req.headers.get("last-event-id"));
  }

  // ---- actors -------------------------------------------------------------
  if (path === "/v1/actors" && method === "GET") {
    const after = url.searchParams.get("after");
    const limit = limitParam(url, "limit", 50);
    if (limit === null) return invalidRequest();
    const start = after ? Math.max(0, store.order.indexOf(after) + 1) : 0;
    const ids = after && store.order.indexOf(after) < 0 ? [] : store.order.slice(start, start + limit);
    const items = ids.map(id => store.actors.get(id)?.summary).filter((summary): summary is NonNullable<typeof summary> => Boolean(summary));
    const next = start + limit < store.order.length && ids.length ? ids[ids.length - 1] : null;
    return json({ items, next_cursor: next } satisfies Page<NonNullable<ReturnType<typeof getActor>>["summary"], string>);
  }
  if ((match = RE_ACTOR.exec(path))) {
    const actor = getActor(decodeSegment(match[1]));
    if (!actor) return notFound("Unknown actor");
    if (method === "GET") return json(actor.detail);
    if (method === "PATCH") {
      const body = await jsonBody(req);
      if (!body) return invalidRequest("Expected a JSON body");
      const config = actor.detail.config;
      if (body.model !== undefined) {
        if (body.model !== null && typeof body.model !== "string") return invalidRequest("model must be a string or null");
        if (config) config.model = body.model as string | null;
      }
      if (body.approval_mode !== undefined) {
        const mode = body.approval_mode;
        if (mode !== "auto" && mode !== "ask" && mode !== "none") return invalidRequest("approval_mode must be auto, ask or none");
        if (config) config.permissions.approval_mode = mode as ApprovalMode;
      }
      const ts = now();
      actor.summary.updated_at_ms = ts;
      actor.detail.updated_at_ms = ts;
      emitEvent(actor.summary.id, { type: "actor_changed", actor: { ...actor.summary } });
      return json(actor.detail);
    }
    return null;
  }
  // Phase 1 has no remote stages; any /v1/stages/... address is unknown.
  if (RE_REMOTE.test(path)) return notFound("Unknown remote stage");

  // ---- messages -----------------------------------------------------------
  if ((match = RE_MESSAGES.exec(path))) {
    const actor = getActor(decodeSegment(match[1]));
    if (!actor) return notFound("Unknown actor");
    if (method === "GET") {
      const before = intParam(url, "before");
      const limit = limitParam(url, "limit", 30);
      if ((before !== null && before < 0) || limit === null) return invalidRequest();
      const page = pageDesc(actor.messages, before, limit);
      return json({ ...page, event_cursor: actor.eventSeq });
    }
    if (method === "POST") {
      // Backend (src/service/mod.rs): optional key, 1..=128 ASCII graphic.
      const key = req.headers.get("idempotency-key");
      if (key !== null && (key.length > 128 || !/^[\x21-\x7e]+$/.test(key)))
        return invalidRequest("Idempotency-Key must be 1-128 ASCII graphic characters");
      const body = await jsonBody(req);
      if (!body || typeof body.content !== "string") return invalidRequest("content must be a string");
      const attachmentIds = JSON.stringify(Array.isArray(body.attachments) ? body.attachments : []);
      if (key) {
        const existing = store.idempotency.get(key);
        if (existing && existing.actor_id === actor.summary.id) {
          // Same key + identical payload replays the record (202 again); a changed payload conflicts.
          if (existing.content === body.content && JSON.stringify(existing.attachments.map(item => item.id)) === attachmentIds)
            return json(existing, 202);
          return conflict();
        }
      }
      const busy = isActorBusy(actor.summary.id);
      const message = addMessage(actor, {
        kind: "request",
        source: { kind: "user" },
        reply_to: { kind: "user" },
        content: body.content,
        status: busy ? "queued" : "running",
        attachments: attachmentsFromIds(body.attachments),
      });
      if (key) store.idempotency.set(key, message);
      emitEvent(actor.summary.id, { type: "message_added", message_id: message.id });
      submitTurn(actor, message);
      return json(message, 202);
    }
    return null;
  }
  if ((match = RE_MESSAGE_SEARCH.exec(path)) && method === "GET") {
    const actor = getActor(decodeSegment(match[1]));
    if (!actor) return notFound("Unknown actor");
    const query = (url.searchParams.get("q") ?? "").toLowerCase();
    const matches = query
      ? actor.messages.filter(message =>
          message.content.toLowerCase().includes(query) || (message.reply ?? "").toLowerCase().includes(query))
      : actor.messages;
    const before = intParam(url, "before");
    const limit = limitParam(url, "limit", 30);
    if ((before !== null && before < 0) || limit === null) return invalidRequest();
    const page = pageDesc(matches, before, limit);
    return json({ ...page, event_cursor: actor.eventSeq });
  }
  if ((match = RE_MESSAGE_ACTION.exec(path)) && method === "POST") {
    const actor = getActor(decodeSegment(match[1]));
    if (!actor) return notFound("Unknown actor");
    const message = findMessage(actor, decodeSegment(match[2]));
    if (!message) return notFound("Unknown message");
    const action = match[3];
    if (action === "cancel") {
      touchMessage(message, { cancel_requested: true });
      const stopped = cancelTurn(message.id);
      if (!stopped && ["queued", "running"].includes(message.status)) {
        touchMessage(message, { status: "cancelled", finished_at_ms: now() });
        emitEvent(actor.summary.id, { type: "message_updated", message_id: message.id, status: "cancelled" });
      }
      return noContent();
    }
    if (message.status !== "queued") return invalidRequest("Only queued messages support this action");
    const body = await jsonBody(req);
    if (!body) return invalidRequest("Expected a JSON body");
    if (typeof body.expected_updated_at_ms === "number" && body.expected_updated_at_ms !== message.updated_at_ms)
      return conflict("Message changed since it was loaded");
    if (action === "edit") {
      if (typeof body.content !== "string") return invalidRequest("content must be a string");
      touchMessage(message, { content: body.content, attachments: attachmentsFromIds(body.attachments) });
      return json(message);
    }
    if (action === "withdraw") {
      touchMessage(message, { status: "cancelled", finished_at_ms: now() });
      emitEvent(actor.summary.id, { type: "message_updated", message_id: message.id, status: "cancelled" });
      return json(message);
    }
    // steer: re-target a queued message as steering input for a running turn.
    if (typeof body.target_message_id !== "string") return invalidRequest("target_message_id must be a string");
    touchMessage(message, { kind: "steering", in_reply_to: body.target_message_id });
    return json(message);
  }
  if ((match = RE_ACTIVITY.exec(path)) && method === "GET") {
    const actor = getActor(decodeSegment(match[1]));
    if (!actor) return notFound("Unknown actor");
    const messageId = decodeSegment(match[2]);
    if (!findMessage(actor, messageId)) return notFound("Unknown message");
    const matches = actor.events.filter(record => {
      const event = record.event as { message_id?: string };
      return typeof event.message_id === "string" && event.message_id === messageId;
    });
    const limit = limitParam(url, "limit", 100);
    if (limit === null) return invalidRequest();
    const page = pageAsc(matches, intParam(url, "after"), limit);
    return json({ ...page, event_cursor: actor.eventSeq });
  }
  if ((match = RE_RESULTS.exec(path)) && method === "GET") {
    const actor = getActor(decodeSegment(match[1]));
    if (!actor) return notFound("Unknown actor");
    const messageId = decodeSegment(match[2]);
    const entries = actor.tools.filter(entry => entry.result.message_id === messageId).map(toolSummary);
    const limit = limitParam(url, "limit", 30);
    if (limit === null) return invalidRequest();
    return json(pageDesc(entries, intParam(url, "before"), limit));
  }
  if ((match = RE_RESULT.exec(path)) && method === "GET") {
    const actor = getActor(decodeSegment(match[1]));
    if (!actor) return notFound("Unknown actor");
    const resultId = decodeSegment(match[2]);
    const entry = actor.tools.find(item => item.result.id === resultId);
    return entry ? json(entry.result) : notFound("Unknown tool result");
  }
  if ((match = RE_MESSAGE.exec(path)) && method === "GET") {
    const actor = getActor(decodeSegment(match[1]));
    if (!actor) return notFound("Unknown actor");
    const message = findMessage(actor, decodeSegment(match[2]));
    return message ? json(message) : notFound("Unknown message");
  }

  // ---- queue --------------------------------------------------------------
  if ((match = RE_QUEUE_CLEAR.exec(path)) && method === "POST") {
    const actor = getActor(decodeSegment(match[1]));
    if (!actor) return notFound("Unknown actor");
    let cancelled = 0;
    for (const message of actor.messages)
      if (message.status === "queued" && ["user", "remote_user"].includes(message.source.kind)) {
        touchMessage(message, { status: "cancelled", finished_at_ms: now() });
        emitEvent(actor.summary.id, { type: "message_updated", message_id: message.id, status: "cancelled" });
        cancelled += 1;
      }
    return json({ cancelled_count: cancelled });
  }
  if ((match = RE_QUEUE.exec(path)) && method === "GET") {
    const actor = getActor(decodeSegment(match[1]));
    if (!actor) return notFound("Unknown actor");
    const queued = actor.messages.filter(message => message.status === "queued" && message.kind !== "delivery");
    const active = actor.messages.find(message => message.status === "running" && message.kind === "request") ?? null;
    const limit = limitParam(url, "limit", 24);
    if (limit === null) return invalidRequest();
    const page = pageAsc(queued, intParam(url, "after"), limit);
    return json({ ...page, active });
  }

  // ---- approvals ----------------------------------------------------------
  if (path === "/v1/approvals" && method === "GET") {
    const awaiting = url.searchParams.get("awaiting_owner") === "true";
    const all: ApprovalRecord[] = [];
    for (const actor of store.actors.values()) all.push(...actor.approvals);
    const items = awaiting ? all.filter(record => record.decision === "pending" && record.mode === "ask") : all;
    const limit = limitParam(url, "limit", 30);
    if (limit === null) return invalidRequest();
    const page = pageDesc(items, intParam(url, "before"), limit);
    return json({ ...page, event_cursor: maxSeq(CATALOG_KEY) });
  }
  if ((match = RE_ACTOR_APPROVALS.exec(path)) && method === "GET") {
    const actor = getActor(decodeSegment(match[1]));
    if (!actor) return notFound("Unknown actor");
    const awaiting = url.searchParams.get("awaiting_owner") === "true";
    const items = awaiting
      ? actor.approvals.filter(record => record.decision === "pending" && record.mode === "ask")
      : actor.approvals;
    const limit = limitParam(url, "limit", 30);
    if (limit === null) return invalidRequest();
    const page = pageDesc(items, intParam(url, "before"), limit);
    return json({ ...page, event_cursor: actor.eventSeq });
  }
  if ((match = RE_ACTOR_APPROVAL_ITEM.exec(path))) {
    const actor = getActor(decodeSegment(match[1]));
    if (!actor) return notFound("Unknown actor");
    const approvalId = decodeSegment(match[2]);
    const record = actor.approvals.find(item => item.id === approvalId);
    if (!record) return notFound("Unknown approval");
    if (method === "GET") return json(record);
    if (method === "POST") {
      const body = await jsonBody(req);
      // Backend denies unknown fields: {"decision"} only (src/service/approvals.rs).
      if (!body || Object.keys(body).some(key => key !== "decision")) return invalidRequest();
      const decision = body.decision;
      if (decision !== "approve" && decision !== "deny") return invalidRequest("decision must be approve or deny");
      if (record.mode !== "ask") return conflict("This action does not await an owner decision");
      if (record.decision !== "pending") return conflict("Approval is already resolved");
      const resolved = resolveApprovalRecord(
        record.id,
        decision === "approve" ? "approved" : "denied",
        decision === "approve" ? "Approved by Owner" : "Denied by Owner",
      );
      return resolved ? json(resolved) : notFound("Unknown approval");
    }
    return null;
  }
  if ((match = RE_APPROVAL.exec(path))) {
    const approvalId = decodeSegment(match[1]);
    if (method === "GET") {
      const record = findApproval(approvalId);
      return record ? json(record) : notFound("Unknown approval");
    }
    if (method === "POST") {
      const body = await jsonBody(req);
      if (!body || Object.keys(body).some(key => key !== "decision")) return invalidRequest();
      const decision = body.decision;
      if (decision !== "approve" && decision !== "deny") return invalidRequest("decision must be approve or deny");
      const record = findApproval(approvalId);
      if (!record) return notFound("Unknown approval");
      if (record.mode !== "ask") return conflict("This action does not await an owner decision");
      if (record.decision !== "pending") return conflict("Approval is already resolved");
      const resolved = resolveApprovalRecord(
        approvalId,
        decision === "approve" ? "approved" : "denied",
        decision === "approve" ? "Approved by Owner" : "Denied by Owner",
      );
      return resolved ? json(resolved) : notFound("Unknown approval");
    }
    return null;
  }

  // ---- attachments / resources ---------------------------------------------
  if ((match = RE_ATTACHMENTS.exec(path)) && method === "POST") {
    const actor = getActor(decodeSegment(match[1]));
    if (!actor) return notFound("Unknown actor");
    // Header/limits contract from src/service/resources.rs.
    const rawName = req.headers.get("x-file-name");
    if (!rawName) return invalidRequest("X-File-Name header is required");
    const nameEncoding = req.headers.get("x-file-name-encoding");
    if (nameEncoding !== null && nameEncoding !== "utf-8-percent") return invalidRequest("Unsupported X-File-Name-Encoding");
    const name = decodeSegment(rawName);
    if (!name || name.length > 255 || /[\/\\:]|[\x00-\x1f]/.test(name)) return invalidRequest("Invalid file name");
    const mediaType = (req.headers.get("content-type") || "application/octet-stream").split(";")[0].trim();
    const bytes = new Uint8Array(await req.arrayBuffer());
    if (bytes.byteLength === 0) return invalidRequest("Attachment body must not be empty");
    if (bytes.byteLength > 32 * 1024 * 1024) return json({ message: "Payload exceeds the attachment limit", code: "payload_too_large" }, 413);
    if (mediaType.startsWith("image/") && bytes.byteLength > 10 * 1024 * 1024) return invalidRequest("Image attachments are limited to 10 MiB");
    const id = newId();
    const attachment: Attachment = {
      id,
      name,
      media_type: mediaType,
      size_bytes: bytes.byteLength,
      uri: `/v1/actors/${actor.summary.id}/resources/${id}`,
    };
    store.attachments.set(id, { attachment, bytes, media_type: mediaType });
    return json(attachment, 201);
  }
  if ((match = RE_RESOURCE.exec(path)) && (method === "GET" || method === "HEAD")) {
    const actor = getActor(decodeSegment(match[1]));
    if (!actor) return notFound("Unknown actor");
    const blob = store.attachments.get(decodeSegment(match[2]));
    if (!blob) return notFound("Unknown resource");
    // Response-header contract from src/service/resources.rs.
    const headers = new Headers({
      "Content-Type": blob.media_type,
      "Content-Length": String(blob.bytes.byteLength),
      "Cache-Control": "private, no-store",
      "X-Content-Type-Options": "nosniff",
      "Referrer-Policy": "no-referrer",
      "Content-Security-Policy": "sandbox allow-scripts; default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data: blob:; connect-src 'none'; form-action 'none'; base-uri 'none'; frame-ancestors 'self'",
    });
    const INLINE_TYPES = new Set(["text/html", "image/svg+xml", "image/png", "image/jpeg", "image/webp", "image/gif", "application/pdf", "text/plain"]);
    if (!INLINE_TYPES.has(blob.media_type)) headers.set("Content-Disposition", "attachment");
    return method === "HEAD" ? new Response(null, { status: 200, headers }) : new Response(blob.bytes as unknown as BodyInit, { status: 200, headers });
  }

  // ---- config --------------------------------------------------------------
  if (path === "/v1/config" && method === "GET") return json(configHolder.current);
  if (path === "/v1/config" && method === "PUT") {
    const body = await jsonBody(req);
    if (!body || !Array.isArray(body.models)) return invalidRequest("Expected a SetupConfigRequest body");
    configHolder.current = body as unknown as SetupConfigRequest;
    return json(readiness());
  }
  if (path === "/v1/config/preview" && method === "POST") {
    const body = await jsonBody(req);
    if (!body || !Array.isArray(body.models)) return invalidRequest("Expected a SetupConfigRequest body");
    return json(readiness());
  }
  if (path === "/v1/config/readiness" && method === "GET") return json(readiness());
  if (path === "/v1/config/models/discover" && method === "POST") return json({ models: [], source: "catalog" });
  if (/^\/v1\/config\/auth\/(run|start|complete)$/.test(path) && method === "POST")
    return notFound("Provider authentication is not available in the mock server");

  return null;
}
