import { Storage } from "nsh";
import { expect, test } from "nsh/test";

import { createSettingsStore, DEFAULT_SETTINGS } from "./settings";

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
  store.update({ appearance: "dark" });
  await new Promise((resolve) => setTimeout(resolve, 400));
  const reopened = createSettingsStore();
  await reopened.load();
  expect(reopened.settings().appearance).toBe("dark");
});

test("appearance_and_language_ignore_legacy_sharing_values", async () => {
  const store = createSettingsStore();
  for (const value of [true, "true", false]) {
    await Storage.setItem("settings", JSON.stringify({ appearance: "dark", language: "en", remoteControlEnabled: value }));
    await store.load();
    expect(store.settings()).toEqual({ appearance: "dark", language: "en" });
  }
});
