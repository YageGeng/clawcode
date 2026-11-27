import { colorScheme, computed, signal } from "nsh";

/** Concrete file-icon packs selectable directly or as light/dark dynamic targets. */
export const ICON_THEMES = [
  "catppuccin-latte",
  "catppuccin-frappe",
  "catppuccin-macchiato",
  "catppuccin-mocha",
  "minimal",
] as const;

/** Name of a concrete file-icon pack. */
export type IconThemeName = (typeof ICON_THEMES)[number];

/** Top-level icon theme setting: `dynamic` switches between light and dark packs, or pins one pack directly. */
export const ICON_THEME_SELECTIONS = ["dynamic", ...ICON_THEMES] as const;

/** Top-level icon theme selection. */
export type IconThemeSelection = (typeof ICON_THEME_SELECTIONS)[number];

/** How `dynamic` chooses between `lightIconTheme` and `darkIconTheme`. */
export const ICON_THEME_MODES = ["system", "light", "dark"] as const;

/** Mode controlling dynamic icon theme resolution. */
export type IconThemeMode = (typeof ICON_THEME_MODES)[number];

/** Active icon theme configuration applied by `SettingsStore`. */
export type IconThemeConfig = Readonly<{
  iconTheme: IconThemeSelection;
  iconThemeMode: IconThemeMode;
  lightIconTheme: IconThemeName;
  darkIconTheme: IconThemeName;
}>;

/** Default icon theme configuration: Dynamic following appearance (Latte in light, Mocha in dark). */
export const DEFAULT_ICON_THEME_CONFIG: IconThemeConfig = {
  iconTheme: "dynamic",
  iconThemeMode: "system",
  lightIconTheme: "catppuccin-latte",
  darkIconTheme: "catppuccin-mocha",
};

const activeConfig = signal<IconThemeConfig>(DEFAULT_ICON_THEME_CONFIG);

/** Updates the global reactive file-icon configuration immediately when settings load or change. */
export function setIconThemeConfig(config: IconThemeConfig): void {
  activeConfig.set(config);
}

/** Resolves the concrete `IconThemeName` currently in effect from settings and window appearance. */
export const effectiveIconTheme = computed<IconThemeName>(() => {
  const cfg = activeConfig();
  if (cfg.iconTheme !== "dynamic") return cfg.iconTheme;
  const scheme = cfg.iconThemeMode === "system" ? colorScheme() : cfg.iconThemeMode;
  return scheme === "dark" ? cfg.darkIconTheme : cfg.lightIconTheme;
});
