import { describe, expect, test } from "bun:test";
import { actorIdFromUrl, localActorId, resourceActorId, safeLink } from "../src/lib/conversation-links";

describe("conversation links are locators, not capabilities", () => {
  test("only exact same-origin Actor paths open a dialog", () => {
    expect(actorIdFromUrl("/actors/worker-1", "https://dl.example")).toBe("worker-1");
    expect(actorIdFromUrl("https://dl.example/actors/root", "https://dl.example")).toBe("root");
    for (const url of ["https://other.example/actors/root", "/actors/../root", "/actors/root?token=secret", "/actors/root#secret", "file:///actors/root", "/actors/a/b"]) expect(actorIdFromUrl(url, "https://dl.example")).toBeNull();
  });
  test("resource embeds require an exact authorized service resource route", () => {
    expect(resourceActorId("/v1/actors/root/resources/a.png", "https://dl.example")).toBe("root");
    expect(resourceActorId("https://outside.example/a.png", "https://dl.example")).toBeNull();
    expect(resourceActorId("/artifacts/a.png?sig=old", "https://dl.example")).toBeNull();
  });
  test("script and filesystem navigation are never auto-opened", () => {
    for (const target of ["javascript:alert(1)", "file:///C:/secret", "C:\\secret.txt", "//other.example/x", "/\\other.example"]) expect(safeLink(target)).toBeUndefined();
    expect(safeLink("https://example.org/page")).toBe("https://example.org/page");
    expect(safeLink("/actors/root")).toBe("/actors/root");
  });
});

test("remote conversation and resource links keep their exact Stage identity", () => {
  const stage="12345678-1234-1234-1234-123456789012";
  expect(actorIdFromUrl(`/stages/${stage}/actors/task`)).toBe(`${stage}~task`);
  expect(resourceActorId(`/v1/stages/${stage}/actors/task/resources/image`)).toBe(`${stage}~task`);
  expect(actorIdFromUrl(`/stages/${"-".repeat(36)}/actors/task`)).toBeNull();
});

test("a hosted conversation on this device uses its local Actor identity without changing other devices", () => {
  const local = "12345678-1234-1234-1234-123456789012";
  const remote = "87654321-4321-4321-4321-210987654321";
  expect(localActorId(actorIdFromUrl(`/stages/${local}/actors/task`)!, local)).toBe("task");
  expect(localActorId(`${remote}~task`, local)).toBe(`${remote}~task`);
  expect(localActorId(`${local}~task`, null)).toBe(`${local}~task`);
  expect(localActorId("core", local)).toBe("core");
  expect(localActorId(`${local}~task/extra`, local)).toBe(`${local}~task/extra`);
});
