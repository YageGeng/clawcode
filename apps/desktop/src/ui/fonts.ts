import { DesktopFonts } from "clawcode-backend";
import { platform, setTheme } from "nsh";

import theme from "../../theme.json";

/** Applies a verified CJK fallback only on Linux systems whose default face cannot be redrawn. */
export function configureDesktopFonts(): void {
  if (platform.os !== "linux" || typeof DesktopFonts?.fallbacks !== "function") return;
  const fonts = DesktopFonts.fallbacks();
  if (Object.keys(fonts).length > 0) {
    setTheme({ cssVars: { ...theme.cssVars, theme: fonts } });
  }
}
