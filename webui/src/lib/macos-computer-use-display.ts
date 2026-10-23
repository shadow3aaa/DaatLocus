export type MacDisplayConfig = {width: number; height: number; scale: number};
export type ComputerUsePermissions = {enabled: boolean; accessibility: boolean; screen_recording: boolean; display_supported: boolean; display_ready: boolean; display: MacDisplayConfig};
export function validMacDisplay(display: MacDisplayConfig) {
  return Number.isInteger(display.width) && Number.isInteger(display.height) && display.width >= 256 && display.width <= 4096 && display.height >= 256 && display.height <= 4096
    && display.width * display.height <= 8_000_000 && [1,2].includes(display.scale) && display.width % display.scale === 0 && display.height % display.scale === 0;
}
