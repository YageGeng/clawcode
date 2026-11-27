import { setColorScheme, signal, Storage } from "nsh";
import type { Accessor } from "nsh";
import { setLocale } from "nsh/i18n";

import { messageOf } from "../client/errors";
import {
  DEFAULT_ICON_THEME_CONFIG,
  ICON_THEME_MODES,
  ICON_THEME_SELECTIONS,
  ICON_THEMES,
  setIconThemeConfig,
} from "./iconTheme";
import type { IconThemeConfig, IconThemeMode, IconThemeName, IconThemeSelection } from "./iconTheme";

export { ICON_THEME_MODES, ICON_THEME_SELECTIONS, ICON_THEMES };
export type { IconThemeMode, IconThemeName, IconThemeSelection };

/** The window's light/dark choices, in the order the settings list them. */
export const APPEARANCES = ["system", "light", "dark"] as const;
/** The window's light/dark choice. */
export type Appearance = (typeof APPEARANCES)[number];
/** The interface languages, in the order the settings list them; `system` follows the user's languages. */
export const LANGUAGES = ["system", "en", "zh-CN"] as const;
/** The interface language. */
export type Language = (typeof LANGUAGES)[number];

/** What the settings dialog changes. */
export type Settings = IconThemeConfig & Readonly<{
  appearance: Appearance;
  language: Language;
}>;

/** The settings before the user changed any. */
export const DEFAULT_SETTINGS: Settings = {
  appearance: "system",
  language: "system",
  ...DEFAULT_ICON_THEME_CONFIG,
};

/** The `Storage` key the settings live under, as JSON. */
const STORAGE_KEY = "settings";
/** How long changes settle before they are written. */
const SAVE_DELAY_MS = 300;

/** The app's settings: loaded from `Storage`, applied at once, saved on change; `update` is the only writer. */
export type SettingsStore = Readonly<{
  settings: Accessor<Settings>;
  /** Reads the saved settings; missing or unreadable JSON keeps the defaults. */
  load: () => Promise<void>;
  /** Applies a change at once and saves it after a short pause. */
  update: (change: Partial<Settings>) => void;
}>;

/** Picks `candidate` when it belongs to `allowed`, otherwise `fallback`. */
function pickKnown<T extends string>(allowed: readonly T[], candidate: unknown, fallback: T): T {
  return (allowed as readonly unknown[]).includes(candidate) ? (candidate as T) : fallback;
}

/** Keeps only known values from saved JSON. */
function parse(saved: string | null): Settings {
  if (saved === null) return DEFAULT_SETTINGS;
  try {
    const value = JSON.parse(saved) as Partial<Record<keyof Settings, unknown>>;
    return {
      appearance: pickKnown(APPEARANCES, value.appearance, DEFAULT_SETTINGS.appearance),
      language: pickKnown(LANGUAGES, value.language, DEFAULT_SETTINGS.language),
      iconTheme: pickKnown(ICON_THEME_SELECTIONS, value.iconTheme, DEFAULT_SETTINGS.iconTheme),
      iconThemeMode: pickKnown(ICON_THEME_MODES, value.iconThemeMode, DEFAULT_SETTINGS.iconThemeMode),
      lightIconTheme: pickKnown(ICON_THEMES, value.lightIconTheme, DEFAULT_SETTINGS.lightIconTheme),
      darkIconTheme: pickKnown(ICON_THEMES, value.darkIconTheme, DEFAULT_SETTINGS.darkIconTheme),
    };
  } catch {
    return DEFAULT_SETTINGS;
  }
}

/** Puts `settings` into effect: the color scheme, the language, and the file icon theme. */
function apply(settings: Settings): void {
  setColorScheme(settings.appearance);
  setLocale(settings.language === "system" ? null : settings.language);
  setIconThemeConfig(settings);
}

/** Creates the settings store with the defaults; `load` reads the saved ones. */
export function createSettingsStore(): SettingsStore {
  const settings = signal<Settings>(DEFAULT_SETTINGS);
  apply(DEFAULT_SETTINGS);
  let pending: number | undefined;
  return {
    settings,
    load: async () => {
      const loaded = parse(await Storage.getItem(STORAGE_KEY));
      settings.set(loaded);
      apply(loaded);
    },
    update: (change) => {
      const next = { ...settings(), ...change };
      settings.set(next);
      apply(next);
      if (pending !== undefined) clearTimeout(pending);
      pending = setTimeout(() => {
        pending = undefined;
        void Storage.setItem(STORAGE_KEY, JSON.stringify(settings())).catch((error: unknown) =>
          console.warn(`saving settings: ${messageOf(error)}`),
        );
      }, SAVE_DELAY_MS);
    },
  };
}
