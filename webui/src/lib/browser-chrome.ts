/** Relative luminance for an sRGB channel pair array, per WCAG 2.1. */
const linear = (channel: number) => {
  const value = channel / 255;
  return value <= .04045 ? value / 12.92 : ((value + .055) / 1.055) ** 2.4;
};
const luminance = (rgb: number[]) => .2126 * linear(rgb[0]) + .7152 * linear(rgb[1]) + .0722 * linear(rgb[2]);
const contrast = (one: number, other: number) => (Math.max(one, other) + .05) / (Math.min(one, other) + .05);
const hex = (rgb: number[]) => "#" + rgb.map(channel => Math.round(channel).toString(16).padStart(2, "0")).join("");

/** Page tint for browser chrome: the page color plus text, raised and hover
 *  surfaces that stay readable on light, dark, saturated and midtone pages. */
export function browserChrome(background: string | null | undefined) {
  if (!background || !/^#[0-9a-f]{6}$/i.test(background)) return null;
  const rgb = [1, 3, 5].map(index => parseInt(background.slice(index, index + 2), 16));
  const page = luminance(rgb);
  const lightText = contrast(1, page) > contrast(0, page);
  const foreground = lightText ? 1 : 0;
  const blend = (target: number, amount: number) => rgb.map(channel => channel + (target - channel) * amount);
  // Raised and hover surfaces move away from the text color, so their contrast
  // only grows. Pure black and white leave no room in that direction, so those
  // lift toward the text instead, capped at the 4.5:1 body-text ratio.
  const surface = (amount: number) => {
    const away = blend(lightText ? 0 : 255, amount);
    if (Math.max(...away.map((channel, index) => Math.abs(channel - rgb[index]))) >= 6) return hex(away);
    for (let step = amount; step > 0; step -= .01) {
      const lifted = blend(lightText ? 255 : 0, step);
      if (contrast(foreground, luminance(lifted)) >= 4.5) return hex(lifted);
    }
    return hex(rgb);
  };
  return {
    background,
    foreground: lightText ? "#ffffff" : "#000000",
    surface: surface(.09),
    hover: surface(.15),
    scheme: lightText ? "dark" : "light",
  };
}
