// Guards the theme's text contrast (WCAG): secondary text ≥ 4.5:1 and the focus ring ≥ 3:1 on every surface they sit on.
import { expect, test } from "nsh/test";

import theme from "../../theme.json";

type Palette = Record<string, string>;

/** `#rrggbb` or `#rrggbbaa` as [r, g, b, a] in 0..1. */
function rgba(hex: string): [number, number, number, number] {
  const n = (i: number) => parseInt(hex.slice(i, i + 2), 16) / 255;
  return [n(1), n(3), n(5), hex.length === 9 ? n(7) : 1];
}

/** `top` composited over the opaque `bottom`. */
function over(top: string, bottom: string): [number, number, number] {
  const [r, g, b, a] = rgba(top);
  const [br, bg, bb] = rgba(bottom);
  return [r * a + br * (1 - a), g * a + bg * (1 - a), b * a + bb * (1 - a)];
}

/** WCAG relative luminance. */
function luminance([r, g, b]: [number, number, number]): number {
  const lin = (c: number) => (c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4);
  return 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b);
}

/** Contrast of `fg` (possibly translucent) on the opaque `bg`. */
function contrast(fg: string, bg: string): number {
  const a = luminance(over(fg, bg));
  const b = luminance(over(bg, bg));
  return (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05);
}

const surfaces = ["background", "sidebar", "sidebar-accent", "muted", "popover"];

for (const mode of ["light", "dark"] as const) {
  test(`secondary_text_is_readable_in_${mode}`, () => {
    const palette = theme.cssVars[mode] as Palette;
    for (const surface of surfaces) {
      const ratio = contrast(palette["muted-foreground"]!, palette[surface]!);
      expect({ surface, ok: ratio >= 4.5 }).toEqual({ surface, ok: true });
    }
  });

  test(`focus_ring_is_visible_in_${mode}`, () => {
    const palette = theme.cssVars[mode] as Palette;
    for (const surface of ["background", "sidebar", "popover"]) {
      const ratio = contrast(palette.ring!, palette[surface]!);
      expect({ surface, ok: ratio >= 3 }).toEqual({ surface, ok: true });
    }
  });
}
