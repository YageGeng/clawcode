import { setColorScheme, Storage } from "nsh";
import { expect, test } from "nsh/test";

import { createSettingsStore, DEFAULT_SETTINGS } from "../../src/stores/settings";
import { effectiveIconTheme } from "../../src/stores/iconTheme";

test("missing_settings_keep_defaults", async () => {
  await Storage.clear();
  const store = createSettingsStore();
  await store.load();
  expect(store.settings()).toEqual(DEFAULT_SETTINGS);
});

test("corrupt_settings_keep_defaults", async () => {
  await Storage.setItem("settings", "{");
  const store = createSettingsStore();
  await store.load();
  expect(store.settings()).toEqual(DEFAULT_SETTINGS);
});

test("update_saves_after_debounce", async () => {
  await Storage.clear();
  const store = createSettingsStore();
  await store.load();
  store.update({
    appearance: "dark",
    iconTheme: "dynamic",
    iconThemeMode: "dark",
    darkIconTheme: "catppuccin-macchiato",
  });
  await new Promise((resolve) => setTimeout(resolve, 400));
  const reopened = createSettingsStore();
  await reopened.load();
  expect(reopened.settings().appearance).toBe("dark");
  expect(reopened.settings().darkIconTheme).toBe("catppuccin-macchiato");
  expect(effectiveIconTheme()).toBe("catppuccin-macchiato");
});

test("dynamic_and_pinned_icon_themes_follow_mode_and_appearance", async () => {
  await Storage.clear();
  const store = createSettingsStore();
  await store.load();
  setColorScheme("light");
  expect(effectiveIconTheme()).toBe("catppuccin-latte");
  setColorScheme("dark");
  expect(effectiveIconTheme()).toBe("catppuccin-mocha");

  store.update({ lightIconTheme: "catppuccin-frappe", iconThemeMode: "light" });
  expect(effectiveIconTheme()).toBe("catppuccin-frappe");

  store.update({ iconTheme: "minimal" });
  expect(effectiveIconTheme()).toBe("minimal");
});

test("appearance_and_language_ignore_legacy_sharing_values", async () => {
  const store = createSettingsStore();
  for (const value of [true, "true", false]) {
    await Storage.setItem("settings", JSON.stringify({ appearance: "dark", language: "en", remoteControlEnabled: value }));
    await store.load();
    expect(store.settings()).toEqual({ ...DEFAULT_SETTINGS, appearance: "dark", language: "en" });
  }
});

test("settings drop an invalid openIn", async () => {
  /** The `openIn` read back after saving `value`. */
  const reload = async (value: unknown) => {
    await Storage.setItem("settings", JSON.stringify({ openIn: value }));
    const store = createSettingsStore();
    await store.load();
    return store.settings().openIn;
  };
  expect(DEFAULT_SETTINGS.openIn).toBe("file-manager");
  expect(await reload("../x")).toBe("file-manager");
  expect(await reload(123)).toBe("file-manager");
  expect(await reload("zed")).toBe("zed");
});
