import { Storage } from "nsh";
import { open, test } from "nsh/test";

test("choosing_chinese_translates_the_dialog", async () => {
  await Storage.clear();
  const app = await open("../../tests/entries/settings.tsx", { platform: "linux", width: 1000, height: 700 });
  await app.waitFor("text:Settings");
  await app.press("label:Language");
  await app.press("text:简体中文");
  await app.waitFor("text:设置");
});

test("chinese_locale_names_the_close_button", async () => {
  await Storage.clear();
  const app = await open("../../tests/entries/settings.tsx", { platform: "linux", width: 1000, height: 700, locale: "zh-CN" });
  await app.waitFor("label:关闭");
});
