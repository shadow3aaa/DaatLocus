/** Explicit development preview; production permission checks always use native IPC. */
import type { ComputerUsePermissions, MacDisplayConfig } from "../src/lib/macos-computer-use-display";
let status: ComputerUsePermissions = {enabled: false, accessibility: false, screen_recording: false, display_supported: true, display_ready: false, display: {width:1920,height:1080,scale:1}};
export const isMacDesktop = () => true;
export const getComputerUseStatus = async () => structuredClone(status);
export async function configureComputerUse(enabled: boolean) {
  status = {...status, enabled, display_ready: enabled && status.accessibility && status.screen_recording};return structuredClone(status);
}
export async function requestComputerUsePermission(kind: "accessibility" | "screen_recording") {
  status = {...status, [kind]: true};
}

export async function configureComputerUseDisplay(display: MacDisplayConfig) {status = {...status, display};return structuredClone(status);}
export { validMacDisplay } from "../src/lib/macos-computer-use-display";
