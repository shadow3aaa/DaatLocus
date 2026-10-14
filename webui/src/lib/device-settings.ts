import type { SshTarget } from "./stage-api";

export type SshDraft = { key: string; stage: string; host: string; user: string | null; ssh_port: string; stage_port: string };

/** Only complete, distinct targets may replace the saved SSH configuration. */
export function sshTargets(rows: SshDraft[]): Record<string, SshTarget> | null {
  const targets: Record<string, SshTarget> = {};
  for (const row of rows) {
    const port = Number(row.ssh_port), stagePort = Number(row.stage_port);
    if (!row.stage || Object.hasOwn(targets, row.stage) || !row.host.trim() ||
      !Number.isInteger(port) || port < 1 || port > 65535 ||
      !Number.isInteger(stagePort) || stagePort < 1 || stagePort > 65535) return null;
    targets[row.stage] = { host: row.host.trim(), user: row.user, ssh_port: port, stage_port: stagePort };
  }
  return targets;
}

export function validConnection(value: string): boolean {
  if (!value) return true;
  try {
    const url = new URL(value);
    return url.protocol === "wss:" && url.pathname === "/" && !url.search && !url.hash && !url.username && !url.password;
  } catch { return false; }
}
