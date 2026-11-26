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
  expect(await Storage.getItem("settings")).toBe('{"appearance":"dark","language":"system"}');
});
