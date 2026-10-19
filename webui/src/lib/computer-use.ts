import { request, type FetchOptions } from "./actor-api";

export type ComputerUseDisplay = { width: number; height: number };
export type ComputerUseStatus = {
  supported: boolean;
  enabled: boolean;
  state: "disabled" | "ready" | "unavailable" | "unsupported";
  display: ComputerUseDisplay;
  modes: ComputerUseDisplay[];
};
export type ComputerUsePatch = { enabled?: boolean; display?: ComputerUseDisplay };
export const getComputerUseCapabilities = (options?: FetchOptions) => request<{supported: boolean}>("/v1/device/computer-use/capabilities", options);
export const getComputerUseStatus = (options?: FetchOptions) => request<ComputerUseStatus>("/v1/device/computer-use", options);
export const configureComputerUse = (patch: ComputerUsePatch) =>
  request<ComputerUseStatus>("/v1/device/computer-use", { method: "PATCH", body: patch });
