import { describe, expect, test } from "bun:test";
import type { SessionSummary } from "../src/lib/actor-api";
import { applySessionEvent, nextSessionSource, sessionBoundary, visibleSessions, type SessionSource } from "../src/lib/session-list";

const session = (id: string, at: number, status: SessionSummary["status"] = "idle"): SessionSummary => ({
  id, stage_id: "stage", title: id, status, working_actors: 0, model: null,
  created_at_ms: at, updated_at_ms: at, last_activity_at_ms: at, conversation_url: `/actors/${id}`,
});
const source = (stage: string | null, items: SessionSummary[], next: string | null, failed = false): SessionSource => ({ stage, label: null, items, next, failed });

describe("merged session list", () => {
  test("rows older than a source's unloaded pages wait for that source", () => {
    const local = source(null, [session("a", 90), session("b", 40)], "cursor");
    const remote = source("remote", [session("c", 70), session("d", 10)], null);
    expect(sessionBoundary([local, remote])?.address).toBe("b");
    expect(visibleSessions([local, remote]).map(entry => entry.address)).toEqual(["a", "remote~c", "b"]);
    expect(nextSessionSource([local, remote])).toBe(local);
  });

  test("failed or exhausted sources do not hold back other rows", () => {
    const local = source(null, [session("a", 90)], null);
    const failed = source("remote", [session("c", 95)], "cursor", true);
    expect(sessionBoundary([local, failed])).toBeNull();
    expect(nextSessionSource([local, failed])).toBeNull();
    expect(visibleSessions([local, failed]).map(entry => entry.session.id)).toEqual(["c", "a"]);
  });

  test("the next page comes from the source whose loaded range ends latest", () => {
    const early = source(null, [session("a", 90), session("b", 80)], "x");
    const late = source("remote", [session("c", 85), session("d", 20)], "y");
    expect(nextSessionSource([early, late])).toBe(early);
    expect(visibleSessions([early, late]).map(entry => entry.session.id)).toEqual(["a", "c", "b"]);
  });

  test("session events reorder, move between filters and delete within the loaded range", () => {
    let active = source(null, [session("a", 90), session("b", 50)], "cursor");
    active = applySessionEvent(active, { type: "session_changed", session: session("b", 100) }, false);
    expect(active.items.map(item => item.id)).toEqual(["b", "a"]);
    active = applySessionEvent(active, { type: "session_changed", session: session("old", 10) }, false);
    expect(active.items.map(item => item.id)).toEqual(["b", "a"]);
    active = applySessionEvent(active, { type: "session_changed", session: session("a", 95, "archived") }, false);
    expect(active.items.map(item => item.id)).toEqual(["b"]);
    const archived = applySessionEvent(source(null, [], null), { type: "session_changed", session: session("a", 95, "archived") }, true);
    expect(archived.items.map(item => item.id)).toEqual(["a"]);
    expect(applySessionEvent(archived, { type: "session_deleted", session_id: "a" }, true).items).toEqual([]);
  });

  test("equal activity times retain the host's descending ID order across pages and devices", () => {
    const local = source(null, [session("z", 90), session("m", 90)], "cursor");
    const remote = source("remote", [session("y", 90), session("a", 90)], null);
    expect(visibleSessions([local, remote]).map(entry => entry.address)).toEqual(["z", "remote~y", "m"]);
    local.items.push(session("b", 90));
    local.next = null;
    expect(visibleSessions([local, remote]).map(entry => entry.address)).toEqual(["z", "remote~y", "m", "b", "remote~a"]);
  });

  test("a catalog update outside the loaded ID range waits for pagination", () => {
    const local = source(null, [session("z", 90), session("m", 90)], "cursor");
    const unchanged = applySessionEvent(local, {type: "session_changed", session: session("a", 90)}, false);
    expect(unchanged.items.map(item => item.id)).toEqual(["z", "m"]);
    const inserted = applySessionEvent(unchanged, {type: "session_changed", session: session("y", 90)}, false);
    expect(inserted.items.map(item => item.id)).toEqual(["z", "y", "m"]);
  });

  test("an emptied pending source is paged before showing rows with unknown order", () => {
    const local = source(null, [], "cursor");
    const remote = source("remote", [session("a", 90)], null);
    expect(visibleSessions([local, remote])).toEqual([]);
    expect(nextSessionSource([local, remote])).toBe(local);
  });
});
