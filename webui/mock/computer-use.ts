/** Explicit preview fixture; this never controls a real display or driver. */
import type { ComputerUsePatch, ComputerUseStatus } from "../src/lib/computer-use";
let status: ComputerUseStatus = {
  supported: true, enabled: true, state: "ready", display: {width: 1920, height: 1080},
  modes: [[1280,720],[1600,900],[1920,1080],[2560,1440],[3840,2160]].map(([width,height]) => ({width, height})),
};
export function getComputerUseStatus() { return structuredClone(status); }
export async function configureComputerUse(patch: ComputerUsePatch) {
  await new Promise(resolve => setTimeout(resolve, 350));
  status = {...status, ...patch, state: (patch.enabled ?? status.enabled) ? "ready" : "disabled"};
  return structuredClone(status);
}
