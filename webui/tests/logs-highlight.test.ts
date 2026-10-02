import { describe, expect, test } from "bun:test";

import { logEntrySegments, type LogSegment } from "../src/components/logs-page";

function entry(raw: string, structured: Partial<Record<string, string>> = {}) {
  return {
    id: "mock",
    raw,
    timestamp: structured.timestamp ?? null,
    level: structured.level ?? null,
    target: structured.target ?? null,
    message: raw,
  };
}

function texts(segments: LogSegment[]) {
  return segments.map((segment) => [segment.kind, segment.text]);
}

describe("logEntrySegments", () => {
  test("splits python-style lines into timestamp, level, target and message", () => {
    const raw =
      "2026-08-09 17:26:03 - INFO - daat_locus.daemon - daemon booted";
    expect(texts(logEntrySegments(entry(raw)))).toEqual([
      ["timestamp", "2026-08-09 17:26:03"],
      ["level", "INFO"],
      ["target", "daat_locus.daemon"],
      ["message", "daemon booted"],
    ]);
  });

  test("splits tracing-style lines into timestamp, level, target and message", () => {
    const raw = "2026-08-09T17:26:03.123Z  ERROR daat_locus.logs: boom";
    expect(texts(logEntrySegments(entry(raw)))).toEqual([
      ["timestamp", "2026-08-09T17:26:03.123Z"],
      ["level", "ERROR"],
      ["target", "daat_locus.logs"],
      ["message", "boom"],
    ]);
  });

  test("normalizes warn and debug levels", () => {
    const warn = logEntrySegments(
      entry("2026-08-09 17:26:03 - WARN - session - slow poll"),
    );
    expect(warn).toContainEqual({ kind: "level", text: "WARNING" });

    const debug = logEntrySegments(
      entry("2026-08-09 17:26:03 - DEBUG - webui - rerender"),
    );
    expect(debug).toContainEqual({ kind: "level", text: "DEBUG" });
  });

  test("keeps unstructured lines as a single message segment", () => {
    expect(texts(logEntrySegments(entry("random unstructured text")))).toEqual([
      ["message", "random unstructured text"],
    ]);
  });
});
