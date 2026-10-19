/** Explicit development preview; production permission checks always use native IPC. */
export type ComputerUsePermissions = {enabled: boolean; accessibility: boolean; screen_recording: boolean};
let status: ComputerUsePermissions = {enabled: false, accessibility: false, screen_recording: false};
export const isMacDesktop = () => true;
export const getComputerUseStatus = async () => structuredClone(status);
export async function configureComputerUse(enabled: boolean) {
  status = {...status, enabled};return structuredClone(status);
}
export async function requestComputerUsePermission(kind: "accessibility" | "screen_recording") {
  status = {...status, [kind]: true};
}
