import { setColorScheme, signal, Storage } from "nsh";
import type { Accessor } from "nsh";
import { setLocale } from "nsh/i18n";

import { messageOf } from "../client/errors";

/** The window's light/dark choices, in the order the settings list them. */
export const APPEARANCES = ["system", "light", "dark"] as const;
/** The window's light/dark choice. */
export type Appearance = (typeof APPEARANCES)[number];
/** The interface languages, in the order the settings list them; `system` follows the user's languages. */
export const LANGUAGES = ["system", "en", "zh-CN"] as const;
/** The interface language. */
export type Language = (typeof LANGUAGES)[number];
/** What the settings dialog changes. */
export type Settings = Readonly<{ appearance: Appearance; language: Language }>;

/** The settings before the user changed any. */
export const DEFAULT_SETTINGS: Settings = { appearance: "system", language: "system" };

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

/** Keeps only known values from saved JSON. */
function parse(saved: string | null): Settings {
  if (saved === null) return DEFAULT_SETTINGS;
  try {
    const value = JSON.parse(saved) as Partial<Record<keyof Settings, unknown>>;
    const appearance = (APPEARANCES as readonly unknown[]).includes(value.appearance) ? (value.appearance as Appearance) : DEFAULT_SETTINGS.appearance;
    const language = (LANGUAGES as readonly unknown[]).includes(value.language) ? (value.language as Language) : DEFAULT_SETTINGS.language;
    return { appearance, language };
  } catch {
    return DEFAULT_SETTINGS;
  }
}

/** Puts `settings` into effect: the color scheme and the language. */
function apply(settings: Settings): void {
  setColorScheme(settings.appearance);
  setLocale(settings.language === "system" ? null : settings.language);
}

/** Creates the settings store with the defaults; `load` reads the saved ones. */
export function createSettingsStore(): SettingsStore {
  const settings = signal<Settings>(DEFAULT_SETTINGS);
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
