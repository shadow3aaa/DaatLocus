/** Development-only import responses; production parsing and authorization live in Rust. */
import { CODEX_IMPORT_MEDIA_TYPE } from "../src/lib/import-types";
import { addMessage, createActor, emitEvent, emitSessionChanged, newId, store } from "./state";
import { isActorBusy, submitTurn } from "./engine";

const json = (value: unknown, status = 200) => new Response(JSON.stringify(value), {status, headers: {"Content-Type": "application/json"}});
export async function importsRoute(path: string, req: Request): Promise<Response> {
  const titles = ["Review message scrolling and the composer", "Plan MCP App integration", "Fix mobile navigation", ...Array.from({length:27},(_,index)=>`WebUI layout review ${index+1}`)];
  const entries = titles.map((title,index)=>({id:`codex-${index}`,title,updated_at_ms:Date.now()-index*86400000,archived:index>25}));
  if (req.method === "GET") {
    const query = new URL(req.url).searchParams;
    const matches = entries.filter(entry=>entry.title.toLowerCase().includes((query.get("search")||"").toLowerCase()));
    const after = query.get("after"), start = after ? matches.findIndex(entry=>entry.id===after)+1 : 0;
    const page = matches.slice(start,start+24);
    return json({items:page,next_cursor:matches.length>start+24?page.at(-1)!.id:null});
  }
  if (path === "/v1/imports/codex/preview") {
    const body = await req.json() as {source_id:string};
    const entry = entries.find(item=>item.id===body.source_id);
    if(!entry)return json({code:"not_found"},404);
    const id = `mock-resource-${entry.id}`, bytes = new TextEncoder().encode(JSON.stringify({messages:[{role:"user",text:entry.title}]}));
    store.attachments.set(id,{attachment:{id,name:entry.title,media_type:CODEX_IMPORT_MEDIA_TYPE,size_bytes:bytes.length,uri:`/v1/resources/${id}`},bytes,media_type:CODEX_IMPORT_MEDIA_TYPE});
    return json({resource_id:id,title:entry.title,message_count:18});
  }
  const body = await req.json() as {resource_id: string; session_id?: string | null};
  const source = store.attachments.get(body.resource_id);
  if (!source || source.media_type !== CODEX_IMPORT_MEDIA_TYPE) return json({code: "not_found"}, 404);
  const target = body.session_id ?? null;
  if (target !== null) {
    const session = store.sessions.get(target);
    if (!session) return json({code: "not_found"}, 404);
    if (session.archived) return json({code: "conflict", message: "Session is archived"}, 409);
  }
  const key = `import:${body.resource_id}:${target ?? ""}`;
  const previous = store.idempotency.get(key);
  if (previous) return json(previous, 202);
  const actor = target !== null ? store.actors.get(target)! : createActor({id: newId(), name: "Daat Locus", parent_id: null, apps: [], model: null, workspace: "~", instructions: "", title: source.attachment.name});
  if (target === null) emitSessionChanged(actor.summary.id);
  const message = addMessage(actor, {kind: "request", source: {kind: "user"}, reply_to: {kind: "user"}, content: "Import the attached conversation.", attachments: [{...source.attachment, uri: `/v1/actors/${actor.summary.id}/resources/${source.attachment.id}`}], status: isActorBusy(actor.summary.id) ? "queued" : "running"});
  store.idempotency.set(key, message);
  emitEvent(actor.summary.id, {type: "message_added", message_id: message.id});
  submitTurn(actor, message);
  return json(message, 202);
}
