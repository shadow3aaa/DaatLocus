/** Normalized, defensively parsed Artifact activity payload. */
export type SessionActivityArtifactData = {
  artifactId: string;
  version: number;
  kind: string;
  title: string;
  uri: string;
  mimeType: string;
  localPath: string | null;
  byteLen: number | null;
  description: string | null;
};

/** Daemon-relative URL prefixes the WebUI is allowed to load inline. */
const DAEMON_RELATIVE_PREFIXES = ["/artifacts/", "/dashboard/attachments/"] as const;

/** Only these bitmap data URLs may be rendered inline from agent markdown. */
const ALLOWED_DATA_IMAGE_PATTERN = /^data:image\/(?:png|jpeg|gif|webp)[;,]/i;

function asRecord(value: unknown): Record<string, unknown> | null {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    return null;
  }
  return value as Record<string, unknown>;
}

function readString(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}

function readNumber(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

/**
 * Parse the `Artifact` session activity payload (serde external tagging places
 * it under `{"Artifact": {...}}`). Unknown/extra fields are ignored and every
 * field is read defensively so a malformed backend payload never throws.
 */
export function normalizeSessionActivityArtifact(
  value: unknown,
): SessionActivityArtifactData | null {
  const record = asRecord(value);
  if (!record) {
    return null;
  }

  const artifactId = readString(record.artifact_id)?.trim();
  if (!artifactId) {
    return null;
  }

  const rawVersion = readNumber(record.version);
  const version = rawVersion !== null && rawVersion > 0 ? Math.floor(rawVersion) : 1;

  return {
    artifactId,
    version,
    kind: (readString(record.kind) ?? "artifact").trim().toLowerCase(),
    title: readString(record.title) ?? "",
    uri: readString(record.uri)?.trim() ?? "",
    mimeType: readString(record.mime_type) ?? "",
    localPath: readString(record.local_path),
    byteLen: readNumber(record.byte_len),
    description: readString(record.description),
  };
}

/** Extract (and normalize) the Artifact payload straight from an activity event. */
export function artifactDataFromActivityEvent(
  event: unknown,
): SessionActivityArtifactData | null {
  const record = asRecord(event);
  if (!record) {
    return null;
  }
  return normalizeSessionActivityArtifact(record.Artifact);
}

/** True for `/artifacts/...` and `/dashboard/attachments/...` daemon URLs. */
export function isDaemonRelativeUrl(uri: string): boolean {
  const trimmed = uri.trim();
  return DAEMON_RELATIVE_PREFIXES.some((prefix) => trimmed.startsWith(prefix));
}

/** True for absolute `http(s)://` URLs. */
export function isAbsoluteHttpUrl(value: string): boolean {
  return /^https?:\/\//i.test(value.trim());
}

/**
 * Resolve a daemon-relative artifact URL against the daemon origin the same way
 * other daemon-relative URLs are resolved (`new URL(path, window.location.href)`).
 * Absolute URLs and unknown inputs are returned unchanged.
 */
export function resolveArtifactUrl(
  uri: string,
  baseHref?: string | null,
): string {
  const trimmed = uri.trim();
  if (!trimmed) {
    return "";
  }
  if (isAbsoluteHttpUrl(trimmed)) {
    return trimmed;
  }
  if (isDaemonRelativeUrl(trimmed) && baseHref) {
    try {
      return new URL(trimmed, baseHref).toString();
    } catch {
      return trimmed;
    }
  }
  return trimmed;
}

/** Hostname of an absolute http(s) URL, lowercased, or null when not parseable. */
export function artifactUrlHostname(url: string): string | null {
  const trimmed = url.trim();
  if (!isAbsoluteHttpUrl(trimmed)) {
    return null;
  }
  try {
    return new URL(trimmed).hostname.toLowerCase();
  } catch {
    return null;
  }
}

/** True for loopback hostnames: localhost, 127.0.0.0/8, ::1. */
export function isLoopbackHostname(hostname: string): boolean {
  const host = hostname.trim().toLowerCase().replace(/^\[|\]$/g, "");
  if (!host) {
    return false;
  }
  if (host === "localhost" || host === "::1" || host === "0:0:0:0:0:0:0:1") {
    return true;
  }
  return /^127(?:\.\d{1,3}){3}$/.test(host);
}

/**
 * The loopback/remote-detection helper.
 *
 * Returns true when an artifact URL points at a loopback host (localhost,
 * 127.0.0.1, [::1]) while the WebUI itself is *not* served from a loopback host
 * (remote/tunnel access). In that case the browser of a remote user cannot reach
 * the daemon-local address, so the WebUI must not attempt the iframe and should
 * show an explanatory notice with the link instead.
 */
export function shouldBlockLoopbackArtifactIframe({
  url,
  webUiHostname,
}: {
  url: string;
  webUiHostname?: string | null;
}): boolean {
  const targetHostname = artifactUrlHostname(url);
  if (!targetHostname || !isLoopbackHostname(targetHostname)) {
    return false;
  }

  const webUiHost = (webUiHostname ?? "").trim();
  if (!webUiHost) {
    return false;
  }

  return !isLoopbackHostname(webUiHost);
}

/**
 * Allowlist for agent-authored markdown images. Only daemon-relative URLs
 * (`/artifacts/`, `/dashboard/attachments/`) or `data:image/(png|jpeg|gif|webp)`
 * URLs may become an inline `<img>`; everything else must render as a plain link
 * so agent markdown cannot trigger outbound fetches from the user's browser.
 */
export function isAllowedMarkdownImageSource(src: unknown): src is string {
  if (typeof src !== "string") {
    return false;
  }
  const trimmed = src.trim();
  if (!trimmed) {
    return false;
  }
  return isDaemonRelativeUrl(trimmed) || ALLOWED_DATA_IMAGE_PATTERN.test(trimmed);
}

/** Narrow helper for callers holding the raw union payload type. */
