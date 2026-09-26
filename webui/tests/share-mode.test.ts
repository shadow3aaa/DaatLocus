import { describe, expect, test } from "bun:test";

import {
  filterNavigationForShareMode,
  parseShareIdFromHash,
  readShareMode,
  shareModeAllowsPage,
  stripShareFragment,
} from "../src/lib/share-mode";

describe("parseShareIdFromHash", () => {
  test("extracts the share id from a bare fragment", () => {
    expect(parseShareIdFromHash("#s=abc-123")).toBe("abc-123");
  });

  test("extracts the share id alongside other parameters", () => {
    expect(parseShareIdFromHash("#agent&s=abc-123")).toBe("abc-123");
  });

  test("returns null when no share id is present", () => {
    expect(parseShareIdFromHash("")).toBeNull();
    expect(parseShareIdFromHash("#agent")).toBeNull();
    expect(parseShareIdFromHash("#s=")).toBeNull();
  });
});

describe("readShareMode", () => {
  test("is active when the fragment carries a share id", () => {
    expect(readShareMode("#s=abc", false)).toEqual({ active: true, shareId: "abc" });
  });

  test("is active after a stored exchange even without the fragment", () => {
    expect(readShareMode("#agent", true)).toEqual({ active: true, shareId: null });
  });

  test("is inactive otherwise", () => {
    expect(readShareMode("#agent", false)).toEqual({ active: false, shareId: null });
  });
});

describe("stripShareFragment", () => {
  test("removes only the share parameter", () => {
    expect(stripShareFragment("#s=abc&foo=bar")).toBe("#foo=bar");
  });

  test("collapses to an empty hash when nothing else remains", () => {
    expect(stripShareFragment("#s=abc")).toBe("");
  });
});

describe("navigation filtering", () => {
  test("share mode hides settings, logs, study, and files", () => {
    expect(shareModeAllowsPage("agent")).toBe(true);
    expect(shareModeAllowsPage("status")).toBe(true);
    expect(shareModeAllowsPage("settings")).toBe(false);
    expect(shareModeAllowsPage("logs")).toBe(false);
    expect(shareModeAllowsPage("study")).toBe(false);
  });

  test("filters navigation items by page", () => {
    const items = [
      { page: "agent", labelKey: "a" },
      { page: "status", labelKey: "b" },
      { page: "settings", labelKey: "c" },
      { page: "logs", labelKey: "d" },
    ];
    expect(filterNavigationForShareMode(items).map((item) => item.page)).toEqual([
      "agent",
      "status",
    ]);
  });
});
