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
    expect(store.actors.size).toBe(1);
    expect(store.actors.get(ROOT_ACTOR_ID)!.messages).toHaveLength(0);
    expect(await (await request("/v1/config")).json()).toEqual(configuration);
    expect((await (await request("/v1/stage")).json()).settings).toEqual(stage.settings);
    expect((await request("/v1/account/reset",confirmation)).status).toBe(409);
  });
  test("every listed scene is seedable; invalid names do not reset a scene",async()=>{
    for(const scene of scenarios){expect(activateScenario(scene.id)).toBe(true);expect(store.actors.has(scene.actor)).toBe(true);expect(previewState.id).toBe(scene.id);}
    const prior=previewState.id;expect(activateScenario("unknown")).toBe(false);expect(previewState.id).toBe(prior);
    expect((await request("/__mock")).headers.get("content-type")).toContain("text/html");
  });
  test("completed exchanges attach their reply and activity to the request",()=>{
    activateScenario("work");const actor=store.actors.get("research")!;
    expect(actor.messages).toHaveLength(1);expect(actor.messages[0].reply).toContain("消息区");
    expect(actor.tools.every(tool=>tool.result.message_id===actor.messages[0].id)).toBe(true);
  });
  test("motion preview covers sends, thinking, reply arrival and message removal through normal message facts",async()=>{
    activateScenario("core-motion");
    const core=store.actors.get(ROOT_ACTOR_ID)!;
    expect(core.messages).toHaveLength(1);
    for (let phase=1;phase<=6;phase++) {
      const step=await (await request("/__mock/core-motion/step",{method:"POST"})).json();
      expect(step.phase).toBe(phase);
      if (step.input) expect((await request("/v1/core/messages",{method:"POST",headers:{"content-type":"application/json"},body:JSON.stringify({content:step.input,attachments:[]})})).status).toBe(202);
      if (phase===2) expect(core.messages[1].status).toBe("running");
      if (phase===3) expect(core.messages).toHaveLength(3);
      if (phase===5) expect(core.messages.every(message=>message.status==="completed")).toBe(true);
    }
    expect(core.messages).toHaveLength(2);
    expect(core.events.at(-1)?.event.type).toBe("conversation_restored");
  });
  test("connection fixtures reach the actual fixed core readiness contract",async()=>{
    activateScenario("synchronizing");const stage=await (await request("/v1/stage")).json();
    expect(stage.primary_actor_id).toBe("core");expect(stage.core).toMatchObject({ready:false,synchronized:false,state:"synchronizing"});
    activateScenario("models-empty");expect(await (await request("/v1/config/readiness")).json()).toMatchObject({ready:false,kind:"unconfigured"});
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
  test("Owner input is Core-only while hosted conversation observations remain available",async()=>{
    activateScenario("empty");
    const body={method:"POST",headers:{"content-type":"application/json"},body:JSON.stringify({content:"Hello Core",attachments:[]})};
    expect((await request("/v1/actors/research/messages",body)).status).toBe(405);
    expect((await request("/v1/actors/research/messages")).status).toBe(200);
    const response=await request("/v1/core/messages",body);
    expect(response.status).toBe(202);
    expect((await response.json()).actor_id).toBe(ROOT_ACTOR_ID);
  });});
