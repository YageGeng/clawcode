import { expect, open, test } from "nsh/test";

/** Opens settings from the sidebar with deterministic saved preferences, including on a native host. */
async function openSettings(language: "en" | "zh-CN" = "en") {
  const app = await open("../entries/shell.tsx", {
    platform: "linux", width: 800, height: 560,
    storage: { settings: JSON.stringify({ language }) },
  });
  const sidebarItem = language === "en" ? "text:Settings" : "text:设置";
  await app.waitFor(sidebarItem);
  await app.press(sidebarItem);
  return app;
}

test("choosing_chinese_translates_the_dialog", async () => {
  const app = await openSettings();
  await app.press("text:English");
  await app.press("text:简体中文");
  await app.waitFor("text:设置");
  await app.waitFor("text:图标主题");
  await app.waitFor("text:动态切换");
});

test("icon_theme_switches_between_dynamic_suboptions_and_pinned_pack", async () => {
  const app = await openSettings();
  await app.press("text:Catppuccin Mocha");
  await app.press("text:Catppuccin Macchiato");
  await app.waitFor("label:Dark icon theme >> text:Catppuccin Macchiato");
  await app.press("text:Dynamic");
  await app.press("text:Catppuccin Frappé");
  await app.waitFor("label:Icon theme >> text:Catppuccin Frappé");
  expect(app.shows("label:Light icon theme")).toBe(false);
  expect(app.shows("label:Dark icon theme")).toBe(false);
});

test("saved_chinese_language_names_the_close_button", async () => {
  const app = await openSettings("zh-CN");
  await app.waitFor("label:关闭");
  await app.press("label:关闭");
  expect(app.shows("text:图标主题")).toBe(false);
});
