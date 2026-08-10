import { describe, expect, test } from "bun:test";

import {
  logEntryHighlightSpans,
  type LogHighlightSpan,
} from "../src/components/logs-page";

function entry(raw: string) {
  return {
    id: "mock",
    raw,
    timestamp: null,
    level: null,
    target: null,
    message: raw,
  };
}

function spanTexts(raw: string, spans: LogHighlightSpan[]) {
  return spans.map((span) => ({
    text: raw.slice(span.from, span.to),
    className: span.className,
  }));
}

describe("logEntryHighlightSpans", () => {
  test("highlights python-style timestamp, level and target", () => {
    const raw =
      "2026-08-09 17:26:03 - INFO - daat_locus.daemon - daemon booted";
    const spans = logEntryHighlightSpans(entry(raw));

    expect(spanTexts(raw, spans)).toEqual([
      { text: "2026-08-09 17:26:03", className: "log-ts" },
      { text: "INFO", className: "log-level-info" },
      { text: "daat_locus.daemon", className: "log-target" },
    ]);
  });

  test("highlights tracing-style timestamp, level and target", () => {
    const raw = "2026-08-09T17:26:03.123Z  ERROR daat_locus.logs: boom";
    const spans = logEntryHighlightSpans(entry(raw));

    expect(spanTexts(raw, spans)).toEqual([
      { text: "2026-08-09T17:26:03.123Z", className: "log-ts" },
      { text: "ERROR", className: "log-level-error" },
      { text: "daat_locus.logs", className: "log-target" },
    ]);
  });

  test("highlights warn and debug levels with dedicated classes", () => {
    const warnRaw = "2026-08-09 17:26:03 - WARN - session - slow poll";
    const warnSpans = logEntryHighlightSpans(entry(warnRaw));
    expect(spanTexts(warnRaw, warnSpans)).toContainEqual({
      text: "WARN",
      className: "log-level-warn",
    });

    const debugRaw = "2026-08-09 17:26:03 - DEBUG - webui - rerender";
    const debugSpans = logEntryHighlightSpans(entry(debugRaw));
    expect(spanTexts(debugRaw, debugSpans)).toContainEqual({
      text: "DEBUG",
      className: "log-level-debug",
    });
  });

  test("returns no spans for unstructured lines", () => {
    expect(logEntryHighlightSpans(entry("random unstructured text"))).toEqual(
      [],
    );
  });
});
