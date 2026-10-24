import type { ComputerUsePermissions } from "./macos-computer-use-display";

export async function refreshMacAuthorization(bridge: {
  status: () => Promise<ComputerUsePermissions>;
  configure: (enabled: boolean) => Promise<ComputerUsePermissions>;
  canConfigure?: () => boolean;
}) {
  const actual = await bridge.status();
  if (actual.enabled && actual.accessibility && actual.screen_recording && actual.display_supported && !actual.display_ready && (bridge.canConfigure?.() ?? true))
    return bridge.configure(true);
  return actual;
}
