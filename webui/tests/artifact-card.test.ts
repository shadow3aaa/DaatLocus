import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

import "../src/lib/i18n";
import { ArtifactCard } from "../src/components/artifact-card";
import type { SessionActivityArtifactData } from "../src/lib/artifact";

function artifact(
  overrides: Partial<SessionActivityArtifactData> = {},
): SessionActivityArtifactData {
  return {
    artifactId: "a1b2c3",
    version: 2,
    kind: "html",
    title: "Artifact",
    uri: "/artifacts/deadbeef.html?sig=cafe",
    mimeType: "text/html",
    localPath: null,
    byteLen: null,
    description: null,
    ...overrides,
  };
}

const originalWindow = (globalThis as { window?: unknown }).window;

function stubWindow(hostname: string) {
  (globalThis as { window?: unknown }).window = {
    location: {
      hostname,
      href: `https://${hostname}/#agent`,
    },
    localStorage: { getItem: () => null },
  };
}

beforeAll(() => {
  stubWindow("tunnel.example.com");
});

afterAll(() => {
  (globalThis as { window?: unknown }).window = originalWindow;
});

describe("ArtifactCard loopback handling", () => {
  test("renders a notice instead of an iframe for a loopback url while served remotely", () => {
    stubWindow("tunnel.example.com");
    const html = renderToStaticMarkup(
      createElement(ArtifactCard, {
        artifact: artifact({
          kind: "url",
          title: "Dev server",
          uri: "http://localhost:5173/",
        }),
      }),
    );

    expect(html).toContain("loopback address (localhost)");
    expect(html).toContain("http://localhost:5173/");
    expect(html).not.toContain("<iframe");
  });

  test("embeds a loopback url when the WebUI itself is loopback", () => {
    stubWindow("localhost");
    const html = renderToStaticMarkup(
      createElement(ArtifactCard, {
        artifact: artifact({
          kind: "url",
          title: "Dev server",
          uri: "http://localhost:5173/",
        }),
      }),
    );

    expect(html).toContain("<iframe");
    expect(html).toContain(
      'sandbox="allow-scripts allow-forms allow-modals allow-popups"',
    );
    expect(html).not.toContain("allow-same-origin");
    expect(html).toContain("no-referrer");

    stubWindow("tunnel.example.com");
  });

  test("embeds a daemon-relative html artifact through a sandboxed iframe", () => {
    stubWindow("tunnel.example.com");
    const html = renderToStaticMarkup(
      createElement(ArtifactCard, {
        artifact: artifact({ kind: "html", title: "Page" }),
      }),
    );

    expect(html).toContain("<iframe");
    expect(html).toContain(
      'sandbox="allow-scripts allow-forms allow-modals allow-popups"',
    );
    expect(html).not.toContain("allow-same-origin");
    expect(html).toContain("loading=\"lazy\"");
    expect(html).toContain("/artifacts/deadbeef.html?sig=cafe");
  });

  test("renders an image artifact with lazy loading and no iframe", () => {
    stubWindow("tunnel.example.com");
    const html = renderToStaticMarkup(
      createElement(ArtifactCard, {
        artifact: artifact({
          kind: "image",
          title: "Chart",
          mimeType: "image/png",
          uri: "/artifacts/deadbeef.png?sig=cafe",
        }),
      }),
    );

    expect(html).toContain("<img");
    expect(html).toContain('loading="lazy"');
    expect(html).toContain("object-contain");
    expect(html).not.toContain("<iframe");
    expect(html).not.toContain("token=");
  });
});
