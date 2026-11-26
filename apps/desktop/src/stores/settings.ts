import { setColorScheme, signal, Storage } from "nsh";
import type { Accessor } from "nsh";
import { setLocale } from "nsh/i18n";

/** The window's light/dark choice. */
export type Appearance = "system" | "light" | "dark";
/** The interface language; `system` follows the user's languages. */
export type Language = "system" | "en" | "zh-CN";
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
    const appearance = ["system", "light", "dark"].includes(value.appearance as string) ? (value.appearance as Appearance) : DEFAULT_SETTINGS.appearance;
    const language = ["system", "en", "zh-CN"].includes(value.language as string) ? (value.language as Language) : DEFAULT_SETTINGS.language;
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
        void Storage.setItem(STORAGE_KEY, JSON.stringify({ appearance: next.appearance, language: next.language })).catch((error: unknown) =>
          console.warn(`saving settings: ${error instanceof Error ? error.message : String(error)}`),
        );
      }, SAVE_DELAY_MS);
    },
  };
}
