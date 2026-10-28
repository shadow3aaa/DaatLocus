import "../src/lib/i18n";
import { describe, expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { ToolDetail } from "../src/components/tool-details";
import { handle } from "../mock/routes";
import { activateScenario } from "../mock/scenarios";
import { store } from "../mock/state";
import { toolExamples, toolSections } from "../mock/tool-showcase";
import { workActivityTimeline, workTranscriptParts } from "../src/lib/work-timeline";

const request=(path:string)=>handle(new Request(`http://127.0.0.1${path}`));
describe("tool renderer preview catalog",()=>{
  test("each example has a real disclosure, detail result and readable renderer output",()=>{
    expect(new Set(toolExamples.map(item=>item.id)).size).toBe(toolExamples.length);
    for (const section of toolSections) {
      for (const example of section.examples) {
        expect(activateScenario(section.id,example.id)).toBe(true);
        const actor=store.actors.get("research")!;
        expect(actor.messages).toHaveLength(1);
        const message=actor.messages.find(item=>item.id===`showcase-message-${example.id}`)!;
        expect(message.source).toEqual({kind:"actor",actor_id:"core"});
        if (example.status === "queued") continue;
        const started=actor.events.find(record=>record.event.type==="tool_started"&&record.event.message_id===message.id)!;
        expect(started.created_at_ms).toBeGreaterThan(message.started_at_ms!);
        if (example.running) continue;
        const result=actor.tools.find(item=>item.result.message_id===message.id)!.result;
        const html=renderToStaticMarkup(<ToolDetail result={result} onActor={()=>{}} />);
        expect(html).not.toContain("PRIVATE_MODEL_TRANSCRIPT");
        expect(html).not.toContain("PRIVATE_SENTINEL");
        expect(html).not.toContain("javascript:");
        expect(actor.events.some(record=>record.event.type==="tool_finished"&&record.event.result_id===result.id)).toBe(true);
        expect(actor.events.find(record=>record.event.type==="tool_finished"&&record.event.result_id===result.id)!.created_at_ms).toBeLessThan(message.finished_at_ms ?? Infinity);
      }
    }
  });
  test("the catalog retains known failure, image, diff, protocol and empty-state examples",()=>{
    const ids=new Set(toolExamples.map(item=>item.id));
    for (const id of ["file-diff-long","file-binary","file-image-broken","terminal-nonzero","terminal-error","web-json","web-json-invalid","web-mixed","browser-closed","computer-error","phone-error","mcp-error","mcp-empty","artifact-html","artifact-invalid","permission-denied","permission-application_required","permission-approved","sudo-wrapped","sudo-invalid","generic-fields","error-running","error-cancelled","error-interrupted","error-queued","detail-loading","detail-retry","activity-retry"]) expect(ids.has(id)).toBe(true);
  });
  test("single-example links validate before resetting and expose controls for every category",async()=>{
    expect(activateScenario("tools-files","file-diff-long")).toBe(true);
    expect(store.actors.get("research")!.messages).toHaveLength(1);
    expect(activateScenario("tools-files","mcp-error")).toBe(false);
    expect(store.actors.get("research")!.messages[0].id).toBe("showcase-message-file-diff-long");
    const response=await request("/__mock/frame?scene=tools-files&case=file-diff-long&width=320");
    const html=await response.text();
    expect(response.status).toBe(200);
    expect(html).toContain('id="tool-case"');
    expect(html).toContain('case=file-diff-long');
    expect(html).toContain('id="tool-expand"');
    expect((await request("/__mock/frame?scene=tools-files&case=invalid")).status).toBe(422);
  });
  test("tool examples remain in one work record with communication between tool groups",async()=>{
    activateScenario("tools-all");
    const actor=store.actors.get("research")!;
    expect(actor.messages).toHaveLength(1);
    const message=actor.messages[0];
    expect(actor.tools.every(tool=>tool.result.message_id===message.id)).toBe(true);
    expect(workTranscriptParts(actor.messages,actor.messages,actor.events).filter(part=>part.kind==="activity")).toHaveLength(1);
    const groups=workActivityTimeline(actor.events,message).filter(segment=>segment.kind==="activity");
    expect(groups.length).toBeGreaterThan(1);
    expect(workActivityTimeline(actor.events,message).filter(segment=>segment.kind==="message")).toHaveLength(2);
    expect(groups.flatMap(group=>group.callIds)).toEqual(expect.arrayContaining(["showcase-call-file-read","showcase-call-terminal-error","showcase-call-mcp-error"]));
    const frame=await (await request("/__mock/frame?scene=tools-all&width=1000")).text();
    expect(frame).toContain('/__mock/open?scene=tools-all');
    expect(frame).not.toContain('tool-gallery');
  });
  test("failed activity and detail reads can be retried without replaying a tool",async()=>{
    for (const [id,path] of [["detail-retry","/v1/actors/research/results/showcase-result-detail-retry"],["activity-retry","/v1/actors/research/messages/showcase-message-activity-retry/activity"]]) {
      activateScenario("tools-failures",id);
      const actor=store.actors.get("research")!, count=actor.tools.length;
      expect((await request(path)).status).toBe(503);
      expect((await request(path)).status).toBe(200);
      expect(actor.tools).toHaveLength(count);
    }
  });
  test("preview image, HTML and downloadable report resources resolve locally",async()=>{
    activateScenario("tools-artifacts");
    for (const id of ["screenshot","html","report"]) {
      const response=await request(`/v1/actors/research/resources/showcase-${id}`);
      expect(response.status).toBe(200);
      expect((await response.arrayBuffer()).byteLength).toBeGreaterThan(0);
    }
  });
});
