import { describe, expect, test } from "bun:test";
import { handle } from "../mock/routes";
import { activateScenario, previewState, scenarios } from "../mock/scenarios";
import { ROOT_ACTOR_ID, store } from "../mock/state";

const request=(path:string,init?:RequestInit)=>handle(new Request(`http://127.0.0.1${path}`,init));
describe("standalone WebUI preview",()=>{
  test("account reset preview preserves model and device settings and rejects stale confirmation",async()=>{
    activateScenario("work");
    const before = await (await request("/v1/account/reset")).json();
    const configuration = await (await request("/v1/config")).json();
    const stage = await (await request("/v1/stage")).json();
    const confirmation = {method:"POST",headers:{"content-type":"application/json"},body:JSON.stringify({account_id:before.account_id,expected_generation:before.generation})};
    expect((await request("/v1/account/reset",confirmation)).status).toBe(204);
    expect(store.actors.size).toBe(0);
    expect(store.sessions.size).toBe(0);
    expect(await (await request("/v1/config")).json()).toEqual(configuration);
    expect((await (await request("/v1/stage")).json()).settings).toEqual(stage.settings);
    expect((await request("/v1/account/reset",confirmation)).status).toBe(409);
  });
  test("every listed scene is seedable; invalid names do not reset a scene",async()=>{
    for(const scene of scenarios){expect(activateScenario(scene.id)).toBe(true);expect(store.actors.has(scene.actor)).toBe(true);expect(previewState.id).toBe(scene.id);}
    const prior=previewState.id;expect(activateScenario("unknown")).toBe(false);expect(previewState.id).toBe(prior);
    expect((await request("/__mock")).headers.get("content-type")).toContain("text/html");
  });
  test("worker names stay unique within each session and Owner input reaches only session Cores",()=>{
    for(const scene of scenarios){
      activateScenario(scene.id);
      const names=new Set<string>();
      for(const actor of store.actors.values()){
        if(actor.summary.role==="session"){expect(actor.summary.session_id).toBe(actor.summary.id);continue;}
        const key=`${actor.summary.session_id}\n${actor.summary.name.trim().toLowerCase()}`;
        expect(names.has(key)).toBe(false);names.add(key);
        expect(store.actors.get(actor.summary.session_id)?.summary.role).toBe("session");
        for(const message of actor.messages)expect(["user","remote_user"].includes(message.source.kind)).toBe(false);
      }
    }
  });
  test("completed exchanges attach their reply and activity to the request",()=>{
    activateScenario("work");const actor=store.actors.get("research")!;
    expect(actor.messages).toHaveLength(1);expect(actor.messages[0].reply).toContain("conversation area");
    expect(actor.tools.every(tool=>tool.result.message_id===actor.messages[0].id)).toBe(true);
  });
  test("motion preview covers sends, thinking and reply arrival through normal message facts",async()=>{
    activateScenario("core-motion");
    const core=store.actors.get(ROOT_ACTOR_ID)!;
    expect(core.messages).toHaveLength(1);
    for (let phase=1;phase<=5;phase++) {
      const step=await (await request("/__mock/core-motion/step",{method:"POST"})).json();
      expect(step.phase).toBe(phase);
      if (step.input) expect((await request(`/v1/sessions/${ROOT_ACTOR_ID}/messages`,{method:"POST",headers:{"content-type":"application/json"},body:JSON.stringify({content:step.input,attachments:[]})})).status).toBe(202);
      if (phase===2) expect(core.messages[1].status).toBe("running");
      if (phase===3) expect(core.messages).toHaveLength(3);
    }
    expect(core.messages.every(message=>message.status==="completed")).toBe(true);
  });
  test("stage status reports model readiness without a fixed Core",async()=>{
    activateScenario("conversation");
    expect(await (await request("/v1/stage/status")).json()).toMatchObject({model_ready:true});
    expect(await (await request("/v1/stage")).json()).not.toHaveProperty("primary_actor_id");
    activateScenario("models-empty");
    expect(await (await request("/v1/stage/status")).json()).toMatchObject({model_ready:false});
    expect(await (await request("/v1/config/readiness")).json()).toMatchObject({ready:false,kind:"unconfigured"});
    const send=await request(`/v1/sessions/${ROOT_ACTOR_ID}/messages`,{method:"POST",headers:{"content-type":"application/json"},body:JSON.stringify({content:"Hello",attachments:[]})});
    expect(send.status).toBe(503);expect((await send.json()).code).toBe("not_ready");
  });
  test("session list pages by recent activity and separates archived sessions",async()=>{
    activateScenario("sessions-many");
    const seen:string[]=[];let after:string|null=null,last=Infinity;
    do {
      const page:{items:{id:string;status:string;last_activity_at_ms:number}[];next_cursor:string|null}=await (await request(`/v1/sessions?limit=25${after?`&after=${encodeURIComponent(after)}`:""}`)).json();
      for(const session of page.items){expect(session.status).not.toBe("archived");expect(session.last_activity_at_ms).toBeLessThanOrEqual(last);last=session.last_activity_at_ms;seen.push(session.id);}
      after=page.next_cursor;
    } while(after);
    const archived=(await (await request("/v1/sessions?archived=true")).json()).items.map((session:{id:string})=>session.id);
    expect(new Set(seen).size).toBe(seen.length);
    expect(seen.length+archived.length).toBe(store.sessions.size);
    expect(archived).toContain("bubble-theme");expect(seen).not.toContain("bubble-theme");
  });
  test("session cursor pagination uses the same descending ID tie-break as the backend",async()=>{
    activateScenario("sessions-many");
    for(const meta of store.sessions.values())meta.last_activity_at_ms=meta.updated_at_ms=Date.now()+1000;
    const first=await (await request("/v1/sessions?limit=2")).json();
    const second=await (await request(`/v1/sessions?limit=2&after=${encodeURIComponent(first.next_cursor)}`)).json();
    const ids=[...first.items,...second.items].map((item:{id:string})=>item.id);
    expect(ids).toHaveLength(4);
    expect(new Set(ids).size).toBe(4);
    expect(ids.every((id,index)=>index===0||ids[index-1]>id)).toBe(true);
  });
  test("sessions are created idempotently, archived before deletion and deleted with their workers",async()=>{
    activateScenario("empty");
    const create={method:"POST",headers:{"content-type":"application/json","idempotency-key":"new-session-1"},body:JSON.stringify({title:null,model:null})};
    const first=await request("/v1/sessions",create),second=await request("/v1/sessions",create);
    expect(first.status).toBe(201);
    const session=await first.json();expect((await second.json()).id).toBe(session.id);
    expect(session.title).toBeNull();
    const send=(content:string)=>request(`/v1/sessions/${session.id}/messages`,{method:"POST",headers:{"content-type":"application/json"},body:JSON.stringify({content,attachments:[]})});
    expect((await send("Plan the release\nwith details")).status).toBe(202);
    expect((await (await request(`/v1/sessions/${session.id}`)).json()).title).toBe("Plan the release");
    expect((await request(`/v1/sessions/${session.id}`,{method:"DELETE"})).status).toBe(409);
    expect((await (await request(`/v1/sessions/${session.id}/archive`,{method:"POST"})).json()).status).toBe("archived");
    expect((await send("Late message")).status).toBe(409);
    expect((await request(`/v1/sessions/${session.id}`,{method:"DELETE"})).status).toBe(204);
    expect([...store.actors.values()].some(actor=>actor.summary.session_id===session.id)).toBe(false);
    expect((await request(`/v1/sessions/${session.id}`)).status).toBe(404);
  });
  test("long history and approvals paginate instead of returning full fixture state",async()=>{
    activateScenario("long-content");const page=await (await request("/v1/actors/research/messages?limit=12")).json();
    expect(page.items).toHaveLength(12);expect(page.next_cursor).not.toBeNull();
    activateScenario("many-approvals");const approvals=await (await request("/v1/approvals?limit=30")).json();
    expect(approvals.items).toHaveLength(30);expect(approvals.next_cursor).not.toBeNull();
  });
  test("attachment fixtures are fetched through actor resource routes",async()=>{
    activateScenario("attachments");const actor=store.actors.get(ROOT_ACTOR_ID)!;const record=actor.messages.at(-1)!;
    expect(record.attachments).toHaveLength(2);
    for(const attachment of record.attachments){const response=await request(attachment.uri);expect(response.status).toBe(200);expect((await response.arrayBuffer()).byteLength).toBe(attachment.size_bytes);}
  });
  test("login preview withholds access and scenario selection returns a usable conversation URL",async()=>{
    activateScenario("login");expect((await request("/v1/access")).status).toBe(401);
    const response=await request("/__mock/open?scene=work");expect(response.status).toBe(303);expect(response.headers.get("location")).toBe("/actors/research");
    expect((await request("/v1/access")).status).toBe(200);
  });
  test("phone frames use only validated scene names and bounded widths",async()=>{
    const frame=await request("/__mock/frame?scene=work&width=320");
    expect(frame.status).toBe(200);expect(await frame.text()).toContain("width:320px");
    expect((await request("/__mock/frame?scene=unknown")).status).toBe(422);
  });
  test("scope application preview uses Core conversation and rejects direct approval writes",async()=>{
    activateScenario("approval");const actor=store.actors.get("research")!;
    const approval=actor.approvals[0];expect(approval.decision).toBe("denied");
    for(const decision of ["approve","deny"]) {
      const response=await request(`/v1/approvals/${approval.id}`,{method:"POST",headers:{"content-type":"application/json"},body:JSON.stringify({decision})});
      expect(response.status).toBe(405);
    }
    expect(actor.messages.find(item=>item.id===approval.message_id)?.status).toBe("completed");
    expect(store.actors.get(ROOT_ACTOR_ID)!.messages.some(item=>item.reply?.includes("/reports"))).toBe(true);
    expect((await request("/v1/approvals?awaiting_owner=true")).status).toBe(400);
  });
  test("Codex imports start a new session or join an active one",async()=>{
    activateScenario("sessions");
    const post=(path:string,body:unknown)=>request(path,{method:"POST",headers:{"content-type":"application/json"},body:JSON.stringify(body)});
    const {resource_id}=await (await post("/v1/imports/codex/preview",{source_id:"codex-0"})).json();
    const sessions=store.sessions.size;
    const fresh=await (await post("/v1/imports/codex",{resource_id,session_id:null})).json();
    expect(store.sessions.size).toBe(sessions+1);
    expect(store.actors.get(fresh.actor_id)?.summary.role).toBe("session");
    const joined=await post("/v1/imports/codex",{resource_id,session_id:"browser-review"});
    expect(joined.status).toBe(202);
    expect((await joined.json()).actor_id).toBe("browser-review");
    expect(store.sessions.size).toBe(sessions+1);
    expect((await post("/v1/imports/codex",{resource_id,session_id:"bubble-theme"})).status).toBe(409);
  });
  test("Owner input reaches session Cores while worker conversations remain observable",async()=>{
    activateScenario("conversation");
    const body={method:"POST",headers:{"content-type":"application/json"},body:JSON.stringify({content:"Hello Core",attachments:[]})};
    expect((await request("/v1/actors/research/messages",body)).status).toBe(405);
    expect((await request("/v1/sessions/research/messages",body)).status).toBe(404);
    expect((await request("/v1/actors/research/messages")).status).toBe(200);
    const response=await request(`/v1/sessions/${ROOT_ACTOR_ID}/messages`,body);
    expect(response.status).toBe(202);
    expect((await response.json()).actor_id).toBe(ROOT_ACTOR_ID);
  });});
