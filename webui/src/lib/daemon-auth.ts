const DAEMON_TOKEN_STORAGE_KEY = "daat-locus.daemonToken";

type DaemonAuthResult =
  | { ok: true }
  | { ok: false; message: string };

export function getStoredDaemonToken() {
  try {
    return window.localStorage.getItem(DAEMON_TOKEN_STORAGE_KEY) ?? "";
  } catch {
    return "";
  }
}

export function storeDaemonToken(token: string) {
  window.localStorage.setItem(DAEMON_TOKEN_STORAGE_KEY, token);
}

export function clearStoredDaemonToken() {
  window.localStorage.removeItem(DAEMON_TOKEN_STORAGE_KEY);
}

export async function verifyDaemonToken(token: string): Promise<DaemonAuthResult> {
  try {
    const response = await fetch("/dashboard/snapshot", {
      method: "GET",
      headers: {
        Accept: "application/json",
        Authorization: `Bearer ${token}`,
      },
    });

    if (response.ok) {
      return { ok: true };
    }

    if (response.status === 401) {
      return { ok: false, message: "Token verification failed: daemon returned 401 Unauthorized." };
    }

    return {
      ok: false,
      message: `Token verification failed: daemon returned ${response.status}${response.statusText ? ` ${response.statusText}` : ""}.`,
    };
  } catch (error) {
    const reason = error instanceof Error ? error.message : String(error);
    return {
      ok: false,
      message: `Unable to reach the daemon API: ${reason}`,
    };
  }
}
