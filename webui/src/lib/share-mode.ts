/**
 * Front-end share mode.
 *
 * A share is derived from the `#s=<share_id>` URL fragment — the only thing a
 * share link carries, since the 4-digit PIN never travels in the link — or from
 * a flag stored in `sessionStorage` after a successful PIN exchange. The share
 * session cookie itself is `HttpOnly`, so JavaScript cannot read it directly;
 * that is why the storage flag exists.
 */

export const SHARE_FRAGMENT_PARAM = "s";
export const SHARE_MODE_STORAGE_KEY = "daat-locus.shareMode";
export const SHARE_DIALOG_EVENT = "daat-locus:open-share";

export type ShareMode = {
  active: boolean;
  shareId: string | null;
};

export type ShareDialogRequest = {
  sessionId: string | null;
  /** Open with the unrestricted scope pre-selected (global entry). */
  unrestricted?: boolean;
};

/** Pages a share visitor may reach; everything else is hidden from navigation. */
const SHARE_MODE_PAGES = ["agent", "status"] as const;

/** Extract the share id from a location hash such as `#s=abc` or `#agent&s=abc`. */
export function parseShareIdFromHash(hash: string): string | null {
  const raw = hash.startsWith("#") ? hash.slice(1) : hash;
  if (!raw.trim()) {
    return null;
  }
  const params = new URLSearchParams(raw);
  const value = params.get(SHARE_FRAGMENT_PARAM);
  if (!value) {
    return null;
  }
  const trimmed = value.trim();
  return trimmed.length > 0 ? trimmed : null;
}

/** Resolve share mode from the raw hash and the stored post-exchange flag. */
export function readShareMode(hash: string, storedFlag: boolean): ShareMode {
  const shareId = parseShareIdFromHash(hash);
  if (shareId) {
    return { active: true, shareId };
  }
  return { active: storedFlag, shareId: null };
}

/** Remove the `s` parameter from a hash, preserving any other parameters. */
export function stripShareFragment(hash: string): string {
  const raw = hash.startsWith("#") ? hash.slice(1) : hash;
  if (!raw.trim()) {
    return "";
  }
  const params = new URLSearchParams(raw);
  params.delete(SHARE_FRAGMENT_PARAM);
  const next = params.toString();
  return next ? `#${next}` : "";
}

/** Whether the given navigation page is reachable from a share. */
export function shareModeAllowsPage(page: string): boolean {
  return (SHARE_MODE_PAGES as readonly string[]).includes(page);
}

/** Filter navigation items down to the pages a share may visit. */
export function filterNavigationForShareMode<T extends { page: string }>(
  items: readonly T[],
): T[] {
  return items.filter((item) => shareModeAllowsPage(item.page));
}

/** Current share mode for the running page (browser only). */
export function isShareMode(): boolean {
  if (typeof window === "undefined") {
    return false;
  }
  return readShareMode(window.location.hash, hasStoredShareMode()).active;
}

/** The active share id, if any. */
export function currentShareId(): string | null {
  if (typeof window === "undefined") {
    return null;
  }
  return parseShareIdFromHash(window.location.hash);
}

function hasStoredShareMode(): boolean {
  try {
    return window.sessionStorage.getItem(SHARE_MODE_STORAGE_KEY) === "1";
  } catch {
    return false;
  }
}

/** Remember that this tab exchanged a share PIN successfully. */
export function enableShareMode(): void {
  try {
    window.sessionStorage.setItem(SHARE_MODE_STORAGE_KEY, "1");
  } catch {
    // Storage can be unavailable (private mode); share mode still works because
    // the cookie is HttpOnly and sent automatically.
  }
}

/** Open the share dialog for a specific session (or the picker). */
export function requestShareDialog(sessionId: string | null): void {
  if (typeof window === "undefined") {
    return;
  }
  window.dispatchEvent(
    new CustomEvent<ShareDialogRequest>(SHARE_DIALOG_EVENT, {
      detail: { sessionId },
    }),
  );
}
