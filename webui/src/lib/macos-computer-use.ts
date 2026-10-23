import { invoke, isTauri } from "@tauri-apps/api/core";
import type { ComputerUsePermissions, MacDisplayConfig } from "./macos-computer-use-display";
export { validMacDisplay, type ComputerUsePermissions, type MacDisplayConfig } from "./macos-computer-use-display";
export function isMacDesktop() {return typeof window !== "undefined" && isTauri() && (window as Window & {__DAAT_LOCUS_DESKTOP__?: string}).__DAAT_LOCUS_DESKTOP__ === "macos";}
export const getComputerUseStatus = () => invoke<ComputerUsePermissions>("computer_use_status");
export const configureComputerUse = (enabled: boolean) => invoke<ComputerUsePermissions>("computer_use_configure", {enabled});
export const requestComputerUsePermission = (kind: "accessibility" | "screen_recording") => invoke<void>("computer_use_permission", {kind});

export const configureComputerUseDisplay = (display: MacDisplayConfig) => invoke<ComputerUsePermissions>("computer_use_display_configure", {display});
