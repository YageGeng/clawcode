import { Storage } from "nsh";
import { expect, open, test } from "nsh/test";

test("compact_select_widens_popup_for_longer_options", async () => {
  await Storage.clear();
  const app = await open("../entries/shell.tsx", { platform: "linux", width: 1000, height: 700 });
  await app.waitFor("text:What should we build in");
  const trigger = app.layout("label:Workspace");
  expect(trigger).not.toBeNull();
  await app.press("label:Workspace");
  await app.waitFor("text:Choose folder…");
  const option = app.layout("text:Choose folder…");
  expect(option).not.toBeNull();
  expect(option!.clipped).toBe(false);
  expect(option!.width).toBeGreaterThan(trigger!.width);
});

test("wide_field_select_keeps_popup_at_least_trigger_width", async () => {
  await Storage.clear();
  const app = await open("../entries/settings.tsx", { platform: "linux", width: 1000, height: 700 });
  await app.waitFor("label:Appearance");
  const trigger = app.layout("label:Appearance");
  expect(trigger).not.toBeNull();
  await app.press("label:Appearance");
  await app.waitFor("text:Dark");
  const option = app.layout("text:Dark");
  expect(option).not.toBeNull();
  expect(option!.clipped).toBe(false);
  // Option text spans the popup minus popup/item padding and the trailing check icon slot.
  expect(option!.width + 64).toBeGreaterThanOrEqual(trigger!.width);
});
