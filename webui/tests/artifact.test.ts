import { afterAll, beforeAll, describe, expect, test } from "bun:test";

import {
  artifactDataFromActivityEvent,
  artifactUrlHostname,
  isAbsoluteHttpUrl,
  isAllowedMarkdownImageSource,
  isDaemonRelativeUrl,
  isLoopbackHostname,
  normalizeSessionActivityArtifact,
  resolveArtifactUrl,
  shouldBlockLoopbackArtifactIframe,
} from "../src/lib/artifact";
import { getDashboardAttachmentUrl } from "../src/lib/daemon-api";

describe("normalizeSessionActivityArtifact", () => {
  test("parses the flat Artifact payload", () => {
    expect(
      normalizeSessionActivityArtifact({
        artifact_id: "a1b2c3",
        version: 2,
        kind: "image",
        title: "Chart",
        uri: "/artifacts/deadbeef.png?sig=cafe",
        mime_type: "image/png",
        local_path: "C:\\\\tmp\\\\chart.png",
        byte_len: 1234,
        description: "A chart",
      }),
    ).toEqual({
      artifactId: "a1b2c3",
      version: 2,
      kind: "image",
      title: "Chart",
      uri: "/artifacts/deadbeef.png?sig=cafe",
      mimeType: "image/png",
      localPath: "C:\\\\tmp\\\\chart.png",
      byteLen: 1234,
      description: "A chart",
    });
  });

  test("defaults the version to 1 and optional fields to null", () => {
    const artifact = normalizeSessionActivityArtifact({
      artifact_id: "a1b2c3",
      kind: "url",
      title: "Dev server",
      uri: "http://localhost:5173/",
      mime_type: "text/html",
    });

    expect(artifact?.version).toBe(1);
    expect(artifact?.localPath).toBeNull();
    expect(artifact?.byteLen).toBeNull();
    expect(artifact?.description).toBeNull();
  });

  test("ignores unknown and extra fields defensively", () => {
    const artifact = normalizeSessionActivityArtifact({
      artifact_id: "a1b2c3",
      version: 3,
      kind: "SVG",
      title: "Diagram",
      uri: "/artifacts/abc.svg?sig=1",
      mime_type: "image/svg+xml",
      unexpected: { nested: [1, 2, 3] },
    });

    expect(artifact).not.toBeNull();
    expect(artifact?.kind).toBe("svg");
    expect(Object.keys(artifact ?? {})).not.toContain("unexpected");
  });

  test("returns null for malformed payloads", () => {
    expect(normalizeSessionActivityArtifact(null)).toBeNull();
    expect(normalizeSessionActivityArtifact("Artifact")).toBeNull();
    expect(normalizeSessionActivityArtifact([])).toBeNull();
    expect(normalizeSessionActivityArtifact({ version: 2 })).toBeNull();
    expect(
      normalizeSessionActivityArtifact({ artifact_id: "   " }),
    ).toBeNull();
  });
});

describe("artifactDataFromActivityEvent", () => {
  test("extracts a serde externally-tagged Artifact event", () => {
    const artifact = artifactDataFromActivityEvent({
      Artifact: {
        artifact_id: "a1b2c3",
        version: 2,
        kind: "html",
        title: "Page",
        uri: "/artifacts/abc.html?sig=1",
        mime_type: "text/html",
      },
    });

    expect(artifact?.artifactId).toBe("a1b2c3");
    expect(artifact?.kind).toBe("html");
  });

  test("ignores other activity variants", () => {
    expect(artifactDataFromActivityEvent({ RuntimeStatus: { label: "ok" } })).toBeNull();
    expect(artifactDataFromActivityEvent(null)).toBeNull();
    expect(artifactDataFromActivityEvent({ Artifact: null })).toBeNull();
  });
});

describe("isDaemonRelativeUrl / isAbsoluteHttpUrl", () => {
  test("accepts only the self-authorizing daemon URL prefixes", () => {
    expect(isDaemonRelativeUrl("/artifacts/abc.png?sig=1")).toBe(true);
    expect(isDaemonRelativeUrl("/dashboard/attachments/abc.png")).toBe(true);
    expect(isDaemonRelativeUrl("https://example.com/artifacts/abc.png")).toBe(false);
    expect(isDaemonRelativeUrl("//example.com/artifacts/abc.png")).toBe(false);
    expect(isDaemonRelativeUrl("data:image/png;base64,AAAA")).toBe(false);
  });

  test("detects absolute http(s) URLs", () => {
    expect(isAbsoluteHttpUrl("http://localhost:5173/")).toBe(true);
    expect(isAbsoluteHttpUrl("HTTPS://example.com")).toBe(true);
    expect(isAbsoluteHttpUrl("/artifacts/abc.png")).toBe(false);
  });
});

describe("resolveArtifactUrl", () => {
  test("leaves absolute URLs untouched", () => {
    expect(resolveArtifactUrl("https://example.com/x", "https://daemon.local/")).toBe(
      "https://example.com/x",
    );
  });

  test("resolves daemon-relative URLs against the daemon origin", () => {
    expect(
      resolveArtifactUrl(
        "/artifacts/deadbeef.png?sig=cafe",
        "http://localhost:53825/dashboard",
      ),
    ).toBe("http://localhost:53825/artifacts/deadbeef.png?sig=cafe");
  });

  test("returns the trimmed input when there is no base to resolve against", () => {
    expect(resolveArtifactUrl("  /artifacts/deadbeef.png  ")).toBe(
      "/artifacts/deadbeef.png",
    );
  });
});

describe("isLoopbackHostname", () => {
  test("recognizes loopback hosts", () => {
    expect(isLoopbackHostname("localhost")).toBe(true);
    expect(isLoopbackHostname("LOCALHOST")).toBe(true);
    expect(isLoopbackHostname("127.0.0.1")).toBe(true);
    expect(isLoopbackHostname("127.10.20.30")).toBe(true);
    expect(isLoopbackHostname("::1")).toBe(true);
    expect(isLoopbackHostname("[::1]")).toBe(true);
  });

  test("rejects non-loopback hosts", () => {
    expect(isLoopbackHostname("example.com")).toBe(false);
    expect(isLoopbackHostname("10.0.0.5")).toBe(false);
    expect(isLoopbackHostname("192.168.1.20")).toBe(false);
    expect(isLoopbackHostname("")).toBe(false);
  });
});

describe("artifactUrlHostname", () => {
  test("returns the hostname for absolute URLs only", () => {
    expect(artifactUrlHostname("http://localhost:5173/app")).toBe("localhost");
    expect(artifactUrlHostname("https://Example.COM:8443/x")).toBe("example.com");
    expect(artifactUrlHostname("/artifacts/abc.png")).toBeNull();
  });
});

describe("shouldBlockLoopbackArtifactIframe", () => {
  test("blocks a loopback URL while the WebUI is served remotely", () => {
    expect(
      shouldBlockLoopbackArtifactIframe({
        url: "http://localhost:5173/",
        webUiHostname: "tunnel.example.com",
      }),
    ).toBe(true);
    expect(
      shouldBlockLoopbackArtifactIframe({
        url: "http://127.0.0.1:8080/",
        webUiHostname: "10.0.0.9",
      }),
    ).toBe(true);
  });

  test("allows a loopback URL when the WebUI is also loopback", () => {
    expect(
      shouldBlockLoopbackArtifactIframe({
        url: "http://localhost:5173/",
        webUiHostname: "localhost",
      }),
    ).toBe(false);
  });

  test("allows non-loopback and daemon-relative URLs", () => {
    expect(
      shouldBlockLoopbackArtifactIframe({
        url: "https://example.com/",
        webUiHostname: "tunnel.example.com",
      }),
    ).toBe(false);
    expect(
      shouldBlockLoopbackArtifactIframe({
        url: "/artifacts/abc.html?sig=1",
        webUiHostname: "tunnel.example.com",
      }),
    ).toBe(false);
  });

  test("does not block when the WebUI host is unknown", () => {
    expect(
      shouldBlockLoopbackArtifactIframe({ url: "http://localhost:5173/" }),
    ).toBe(false);
  });
});

describe("isAllowedMarkdownImageSource", () => {
  test("allows daemon-relative sources", () => {
    expect(isAllowedMarkdownImageSource("/artifacts/deadbeef.png?sig=cafe")).toBe(true);
    expect(isAllowedMarkdownImageSource("/dashboard/attachments/abc.png")).toBe(true);
  });

  test("allows inline bitmap data URLs only", () => {
    expect(isAllowedMarkdownImageSource("data:image/png;base64,AAAA")).toBe(true);
    expect(isAllowedMarkdownImageSource("data:image/jpeg;base64,AAAA")).toBe(true);
    expect(isAllowedMarkdownImageSource("data:image/gif;base64,AAAA")).toBe(true);
    expect(isAllowedMarkdownImageSource("data:image/webp;base64,AAAA")).toBe(true);
    expect(isAllowedMarkdownImageSource("data:image/svg+xml;base64,AAAA")).toBe(false);
    expect(isAllowedMarkdownImageSource("data:text/html;base64,AAAA")).toBe(false);
  });

  test("rejects remote and script sources", () => {
    expect(isAllowedMarkdownImageSource("https://anywhere.example/pixel.png")).toBe(false);
    expect(isAllowedMarkdownImageSource("http://localhost/pixel.png")).toBe(false);
    expect(isAllowedMarkdownImageSource("//example.com/pixel.png")).toBe(false);
    expect(isAllowedMarkdownImageSource("javascript:alert(1)")).toBe(false);
    expect(isAllowedMarkdownImageSource("")).toBe(false);
    expect(isAllowedMarkdownImageSource(undefined)).toBe(false);
    expect(isAllowedMarkdownImageSource(null)).toBe(false);
  });
});

describe("getDashboardAttachmentUrl", () => {
  const originalWindow = (globalThis as { window?: unknown }).window;

  beforeAll(() => {
    (globalThis as { window?: unknown }).window = {
      localStorage: {
        getItem: (key: string) =>
          key === "daat-locus.daemonToken" ? "secret-token" : null,
      },
    };
  });

  afterAll(() => {
    (globalThis as { window?: unknown }).window = originalWindow;
  });

  test("never appends the daemon token to /artifacts URLs", () => {
    expect(getDashboardAttachmentUrl("/artifacts/deadbeef.png?sig=cafe")).toBe(
      "/artifacts/deadbeef.png?sig=cafe",
    );
  });

  test("appends the daemon token to /dashboard/attachments URLs only", () => {
    expect(getDashboardAttachmentUrl("/dashboard/attachments/abc.png")).toBe(
      "/dashboard/attachments/abc.png?token=secret-token",
    );
    expect(getDashboardAttachmentUrl("https://example.com/x.png")).toBe(
      "https://example.com/x.png",
    );
  });
});
