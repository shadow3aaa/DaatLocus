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
  test("cancelling a running mock request retains one cancelled conversation record",async()=>{
    activateScenario("work");const actor=store.actors.get("research")!;
    const response=await request("/v1/actors/research/messages",{method:"POST",headers:{"content-type":"application/json"},body:JSON.stringify({content:"检查输入框",attachments:[]})});
    const record=await response.json();expect(response.status).toBe(202);
    const until=async(check:()=>boolean)=>{const deadline=Date.now()+2000;while(!check()&&Date.now()<deadline)await new Promise(resolve=>setTimeout(resolve,10));expect(check()).toBe(true);};
    await until(()=>actor.events.some(item=>item.event.type==="generation_delta"&&item.event.message_id===record.id&&item.event.channel==="assistant"&&item.event.text.includes("检查")));
    expect((await request(`/v1/actors/research/messages/${record.id}/cancel`,{method:"POST"})).status).toBe(204);
    await until(()=>actor.messages.find(item=>item.id===record.id)?.status==="cancelled");
    expect(actor.messages.filter(item=>item.id===record.id||item.in_reply_to===record.id)).toHaveLength(1);
    expect(actor.messages.find(item=>item.id===record.id)?.reply).toContain("检查");
  });
});
