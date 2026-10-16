/** Only application conversation routes can open an Actor. A link never carries authority. */
let localDeviceId: string | null = null;
export function configureConversationDevice(id: string | null): void { localDeviceId = id; }
/** A device-qualified link can point back to the service serving this WebUI. */
export function localActorId(id: string, device = localDeviceId): string {
  const actor = device && id.startsWith(`${device}~`) ? id.slice(device.length + 1) : null;
  return actor && /^[a-zA-Z0-9_-]+$/.test(actor) ? actor : id;
}

export function actorIdFromUrl(href: string, origin = "http://localhost"): string | null {
  try {
    const url = new URL(href, origin);
    if (url.protocol !== new URL(origin).protocol || url.host !== new URL(origin).host || url.search || url.hash) return null;
    const match = /^\/actors\/([a-zA-Z0-9_-]+)\/?$/.exec(url.pathname);
    if (match) return match[1];
    const remote = /^\/stages\/([0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12})\/actors\/([a-zA-Z0-9_-]+)\/?$/.exec(url.pathname);
    return remote ? `${remote[1]}~${remote[2]}` : null;
  } catch { return null; }
}

export function resourceActorId(href: string, origin = "http://localhost"): string | null {
  try {
    const url = new URL(href, origin);
    if (url.protocol !== new URL(origin).protocol || url.host !== new URL(origin).host || url.search || url.hash) return null;
    const local = /^\/v1\/actors\/([a-zA-Z0-9_-]+)\/resources\/[^/]+$/.exec(url.pathname);
    if (local) return local[1];
    const remote = /^\/v1\/stages\/([0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12})\/actors\/([a-zA-Z0-9_-]+)\/resources\/[^/]+$/.exec(url.pathname);
    return remote ? `${remote[1]}~${remote[2]}` : null;
  } catch { return null; }
}
export function conversationUrl(id: string): string {
  const remote = /^([0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12})~([a-zA-Z0-9_-]+)$/.exec(id);
  return remote ? `/stages/${remote[1]}/actors/${remote[2]}` : `/actors/${encodeURIComponent(id)}`;
}

export function safeLink(href: string): string | undefined {
  if (/^\/(?!\/)/.test(href) && !href.includes("\\")) return href;
  try {
    const url = new URL(href);
    if (["https:", "http:", "mailto:"].includes(url.protocol) && !url.username && !url.password) return href;
  } catch { /* Local filesystem paths aren't browser navigation. */ }
  return undefined;
}
