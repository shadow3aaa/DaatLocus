export type PhoneUseDisplay = { width: number; height: number; density: number };

export function validPhoneUseDisplay(value: PhoneUseDisplay): boolean {
  return Number.isInteger(value.width) && value.width >= 256 && value.width <= 4096 &&
    Number.isInteger(value.height) && value.height >= 256 && value.height <= 4096 &&
    value.width * value.height <= 8_000_000 &&
    Number.isInteger(value.density) && value.density >= 80 && value.density <= 640;
}
