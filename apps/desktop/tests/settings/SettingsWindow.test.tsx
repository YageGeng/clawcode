import { expect, open, test } from "nsh/test";

/** Opens the settings window from the sidebar with deterministic saved preferences. */
async function openSettings(language: "en" | "zh-CN" = "en", entry = "../entries/shell.tsx") {
  const app = await open(entry, {
    platform: "linux", width: 800, height: 560,
    storage: { settings: JSON.stringify({ language }) },
  });
  const sidebarItem = language === "en" ? "text:Settings" : "text:设置";
  await app.waitFor(sidebarItem);
  await app.press(sidebarItem);
  await app.waitUntil(() => app.window("settings").state()?.visible === true);
  return app;
}

test("the_settings_window_opens_on_general_and_switches_pages", async () => {
  const app = await openSettings();
  await app.waitFor("label:Open workspaces in");
  await app.press("text:Appearance >> nth=0");
  await app.waitFor("text:Icon theme");
  await app.press("text:About");
  await app.waitFor("text:Version 0.1.0 · NSH 0.7.2");
  await app.waitFor("text:Configuration file");
});

test("choosing_chinese_translates_both_windows", async () => {
  const app = await openSettings();
  await app.press("text:English");
  await app.press("text:简体中文");
  await app.waitFor("text:通用 >> nth=0");
  // The main window shares the settings store: its sidebar follows at once.
  await app.waitFor("text:新会话");
});

test("icon_theme_switches_between_dynamic_suboptions_and_pinned_pack", async () => {
  const app = await openSettings();
  await app.press("text:Appearance >> nth=0");
  await app.press("text:Catppuccin Mocha");
  await app.press("text:Catppuccin Macchiato");
  await app.waitFor("label:Dark icon theme >> text:Catppuccin Macchiato");
  await app.press("text:Dynamic");
  await app.press("text:Catppuccin Frappé");
  await app.waitFor("label:Icon theme >> text:Catppuccin Frappé");
  expect(app.shows("label:Light icon theme")).toBe(false);
  expect(app.shows("label:Dark icon theme")).toBe(false);
});

test("closing_the_window_and_opening_settings_again_reopens_it", async () => {
  const app = await openSettings();
  await app.window("settings").pressClose();
  await app.waitUntil(() => !app.shows("label:Open workspaces in"));
  await app.press("text:Settings");
  await app.waitFor("label:Open workspaces in");
});

test("providers_list_the_local_sign_in_methods_with_their_status", async () => {
  const app = await openSettings("en", "../entries/settings-providers.tsx");
  await app.press("text:Providers");
  await app.waitFor("text:ChatGPT");
  await app.waitFor("text:Signed in");
  expect(app.shows("text:Use your ChatGPT plan")).toBe(true);
  expect(app.shows("text:GitHub Copilot")).toBe(true);
  expect(app.shows("text:Not signed in")).toBe(true);
});

test("signing_in_shows_the_device_code_and_ends_signed_in", async () => {
  const app = await openSettings("en", "../entries/settings-sign-in.tsx");
  await app.press("text:Providers");
  await app.waitFor("text:Not signed in");
  await app.press("text:Sign in");
  await app.waitFor("text:ABCD-1234");
  expect(app.shows("label:Copy device code")).toBe(true);
  expect(app.shows("text:Open in browser")).toBe(true);
  // The scripted host completes the sign-in on its own, as a browser authorization would.
  await app.waitFor("text:Signed in", { timeout: 5000 });
  expect(app.shows("text:ABCD-1234")).toBe(false);
});

/** The page remounted during login must follow completion owned by the shared host. */
test("returning_to_providers_during_sign_in_refreshes_the_completed_status", async () => {
  const app = await openSettings("en", "../entries/settings-sign-in.tsx");
  await app.press("text:Providers");
  await app.waitFor("text:Not signed in");
  await app.press("text:Sign in");
  await app.waitFor("text:ABCD-1234");
  await app.press("text:General");
  await app.waitFor("label:Open workspaces in");
  await app.press("text:Providers");
  // Returning before completion proves that a new page owns the remaining visible updates.
  await app.waitFor("text:ABCD-1234");
  await app.waitFor("text:Signed in", { timeout: 5000 });
  expect(app.shows("text:Not signed in")).toBe(false);
  expect(app.shows("text:ABCD-1234")).toBe(false);
});

test("cancelling_a_sign_in_takes_the_code_away", async () => {
  const app = await openSettings("en", "../entries/settings-sign-in.tsx");
  await app.press("text:Providers");
  await app.press("text:Sign in");
  await app.waitFor("text:ABCD-1234");
  await app.press("text:Cancel");
  await app.waitUntil(() => !app.shows("text:ABCD-1234"));
  await app.waitFor("text:Sign in");
});

test("a_prompt_that_needs_sign_in_offers_it_in_the_notice", async () => {
  const app = await open("../entries/settings-sign-in.tsx", { platform: "linux", width: 1100, height: 700 });
  await app.waitFor("label:Message");
  await app.type("label:Message", "hello");
  await app.submit("label:Message");
  await app.waitFor("text:Sign in to GitHub Copilot to continue");
  await app.press("text:Sign in");
  // The notice opens Providers in the settings window and starts that sign-in.
  await app.waitFor("text:ABCD-1234");
});

test("a_run_whose_credential_was_rejected_offers_a_sign_in", async () => {
  const app = await open("../entries/settings-expired-sign-in.tsx", { platform: "linux", width: 1100, height: 700 });
  await app.waitFor("label:Message");
  await app.type("label:Message", "hello");
  await app.submit("label:Message");
  await app.waitFor("text:Sign in again to continue");
  expect(app.shows("text:provider request failed: provider_response (HTTP 401)")).toBe(true);
  await app.press("text:Sign in");
  await app.waitFor("text:WXYZ-9876");
});
