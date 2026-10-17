/** Development-only import responses; production parsing and authorization live in Rust. */
import { CODEX_IMPORT_MEDIA_TYPE } from "../src/lib/import-types";
import { addMessage, emitEvent, getActor, ROOT_ACTOR_ID, store } from "./state";
import { isActorBusy, submitTurn } from "./engine";

const json = (value: unknown, status = 200) => new Response(JSON.stringify(value), {status, headers: {"Content-Type": "application/json"}});
export async function importsRoute(path: string, req: Request): Promise<Response> {
  const titles = ["检查消息滚动与输入框布局", "整理 MCP App 接入方案", "修复移动端导航", ...Array.from({length:27},(_,index)=>`WebUI 布局检查 ${index+1}`)];
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
    store.attachments.set(id,{attachment:{id,name:entry.title,media_type:CODEX_IMPORT_MEDIA_TYPE,size_bytes:bytes.length,uri:`/v1/actors/${ROOT_ACTOR_ID}/resources/${id}`},bytes,media_type:CODEX_IMPORT_MEDIA_TYPE});
    return json({resource_id:id,title:entry.title,message_count:18});
  }
  const body = await req.json() as {resource_id: string};
  const source = store.attachments.get(body.resource_id);
  if (!source || source.media_type !== CODEX_IMPORT_MEDIA_TYPE) return json({code: "not_found"}, 404);
  const previous = store.idempotency.get(`import:${body.resource_id}`);
  if (previous) return json(previous, 202);
  const actor = getActor(ROOT_ACTOR_ID)!;
  const message = addMessage(actor, {kind: "request", source: {kind: "user"}, reply_to: {kind: "user"}, content: "Prepare an idle Actor from this imported Codex conversation. Do not send it work.", attachments: [source.attachment], status: isActorBusy(ROOT_ACTOR_ID) ? "queued" : "running"});
  store.idempotency.set(`import:${body.resource_id}`, message);
  emitEvent(ROOT_ACTOR_ID, {type: "message_added", message_id: message.id});
  submitTurn(actor, message);
  return json(message, 202);
}
