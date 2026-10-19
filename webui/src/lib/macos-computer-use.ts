import { invoke, isTauri } from "@tauri-apps/api/core";
export type ComputerUsePermissions = {enabled: boolean; accessibility: boolean; screen_recording: boolean};
export function isMacDesktop() {return typeof window !== "undefined" && isTauri() && (window as Window & {__DAAT_LOCUS_DESKTOP__?: string}).__DAAT_LOCUS_DESKTOP__ === "macos";}
export const getComputerUseStatus = () => invoke<ComputerUsePermissions>("computer_use_status");
export const configureComputerUse = (enabled: boolean) => invoke<ComputerUsePermissions>("computer_use_configure", {enabled});
export const requestComputerUsePermission = (kind: "accessibility" | "screen_recording") => invoke<void>("computer_use_permission", {kind});
