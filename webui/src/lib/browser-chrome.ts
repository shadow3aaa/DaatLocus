/** Use the higher WCAG contrast, including saturated and midtone page colors. */
export function browserChrome(background: string | null | undefined) {
  if (!background || !/^#[0-9a-f]{6}$/i.test(background)) return null;
  const channels = [1, 3, 5].map(index => parseInt(background.slice(index, index + 2), 16) / 255)
    .map(channel => channel <= .04045 ? channel / 12.92 : ((channel + .055) / 1.055) ** 2.4);
  const luminance = .2126 * channels[0] + .7152 * channels[1] + .0722 * channels[2];
  const white = 1.05 / (luminance + .05), black = (luminance + .05) / .05;
  return { background, foreground: white > black ? "#ffffff" : "#000000" };
}
