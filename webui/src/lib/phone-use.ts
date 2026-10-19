import { invoke } from "@tauri-apps/api/core";
import type { PhoneUseDisplay } from "./phone-use-display";

export type PhoneUseMethod = "shizuku" | "root";
export type PhoneUseStatus = {
  enabled: boolean;
  method: PhoneUseMethod;
  state: "disabled" | "disconnected" | "connecting" | "ready" | "unsupported" | "unavailable";
  uid: number | null;
  display: PhoneUseDisplay;
};
export const getPhoneUseStatus = () => invoke<PhoneUseStatus>("phone_use_status");
export const configurePhoneUse = (enabled: boolean, method: PhoneUseMethod) =>
  invoke<PhoneUseStatus>("phone_use_configure", { enabled, method });
export const configurePhoneUseDisplay = (patch: Partial<PhoneUseDisplay>) =>
  invoke<PhoneUseStatus>("phone_use_display_configure", { patch });
