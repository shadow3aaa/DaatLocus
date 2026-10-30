import type { CSSProperties } from "react";
import { Avatar, AvatarFallback } from "@/components/ui/avatar";

// Identity is the seed: renaming or reordering a contact never changes its face.
function randomFrom(seed: string) {
  let state = 2166136261;
  for (let i = 0; i < seed.length; i++) state = Math.imul(state ^ seed.charCodeAt(i), 16777619);
  return (count: number) => {
    state = Math.imul(state ^ state >>> 16, 2246822507);
    state = Math.imul(state ^ state >>> 13, 3266489909);
    state ^= state >>> 16;
    return (state >>> 0) % count;
  };
}

const palettes = [
  ["#dfece5", "#87bba2", "#304d42"],
  ["#e1eaf3", "#8eb4d3", "#304c66"],
  ["#f5e7da", "#deb08b", "#654936"],
  ["#ebe4f0", "#b8a0ce", "#51415f"],
  ["#f4e0e3", "#d99ca9", "#653f4a"],
  ["#eeeccd", "#c0bf79", "#535330"],
] as const;

export function GeneratedAvatar({ seed, className, label }: { seed: string; className?: string; label?: string }) {
  const pick = randomFrom(seed);
  const [paletteBackground, paletteShell, paletteInk] = palettes[pick(palettes.length)];
  const background = label ? "var(--avatar-background)" : paletteBackground;
  const shell = label ? "var(--avatar-shell)" : paletteShell;
  const ink = label ? "var(--avatar-ink)" : paletteInk;
  const shape = pick(3), antenna = pick(3), eyes = pick(4), mouth = pick(3);
  const tilt = pick(9) - 4;
  return <Avatar className={className} style={label ? { "--avatar-palette-background": paletteBackground, "--avatar-palette-shell": paletteShell, "--avatar-palette-ink": paletteInk } as CSSProperties : undefined}><AvatarFallback>
    <svg viewBox="0 0 64 64" className="size-full" aria-hidden="true" focusable="false">
      <rect width="64" height="64" fill={background} />
      <g transform={`rotate(${tilt} 32 34)`} stroke={ink} strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
        {antenna === 0 ? <><path d="M32 20V11" /><circle cx="32" cy="9" r="3.5" fill={shell} /></> : antenna === 1 ? <><path d="M22 20L17 12M42 20L47 12" /><circle cx="16" cy="10" r="3" fill={shell} /><circle cx="48" cy="10" r="3" fill={shell} /></> : <path d="M25 20V12Q32 5 39 12V20" fill={shell} />}
        <rect x="8" y="29" width="9" height="14" rx="4" fill={shell} />
        <rect x="47" y="29" width="9" height="14" rx="4" fill={shell} />
        <rect x="14" y="18" width="36" height="34" rx={shape === 0 ? 12 : shape === 1 ? 7 : 17} fill={shell} />
        <rect x="19" y="25" width="26" height="21" rx="9" fill={background} stroke="none" />
        {eyes === 0 ? <><circle cx="26" cy="33" r="2.5" fill={ink} stroke="none" /><circle cx="38" cy="33" r="2.5" fill={ink} stroke="none" /></> : eyes === 1 ? <><path d="M23 33Q26 28 29 33M35 33Q38 28 41 33" fill="none" /></> : eyes === 2 ? <><circle cx="26" cy="33" r="2.5" fill={ink} stroke="none" /><path d="M35 33H41" /></> : <><path d="M24 30V34M40 30V34" /><circle cx="32" cy="32" r="6" fill="none" strokeWidth="1.5" opacity=".35" /></>}
        {mouth === 0 ? <path d="M28 40Q32 44 36 40" fill="none" /> : mouth === 1 ? <path d="M29 40H35" /> : <ellipse cx="32" cy="40" rx="2" ry="2.5" fill={ink} stroke="none" />}
      </g>
    </svg>
    {label && <small title={label}>{label}</small>}
  </AvatarFallback></Avatar>;
}
