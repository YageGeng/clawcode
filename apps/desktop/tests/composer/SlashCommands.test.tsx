import { expect, open, test } from "nsh/test";

import { DESKTOP } from "../support/desktop";

test("slash_keyboard_completion_inserts_the_selected_command_before_enter_sends", async () => {
  const app = await open("../entries/slash-commands.tsx", DESKTOP);
  await app.waitFor("text:Fix the build >> nth=2");
  await app.type("label:Message", "/st");
  await app.waitFor("text:/status");
  await app.keyboard.press("down");
  await app.submit("label:Message");
  expect(app.value("label:Message")).toBe("/status");
  expect(app.shows("label:Available commands")).toBe(false);
  await app.submit("label:Message");
  await app.waitFor("text:Received /status");
});

test("slash_on_a_new_session_prepares_its_own_catalog_and_keeps_arguments_editable", async () => {
  const app = await open("../entries/slash-commands.tsx", DESKTOP);
  await app.waitFor("text:Fix the build >> nth=2");
  await app.press("text:New session");
  await app.type("label:Message", "/na");
  await app.waitFor("text:/name");
  expect(app.shows("text:/status")).toBe(false);
  await app.press("text:/name");
  expect(app.value("label:Message")).toBe("/name ");
  await app.type("label:Message", "My title");
  await app.submit("label:Message");
  await app.waitFor("text:Received /name My title");
});

test("a_pending_slash_request_follows_the_workspace_and_ignores_the_earlier_result", async () => {
  const app = await open("../entries/draft-navigation.tsx", {
    ...DESKTOP, modules: { "clawcode-backend": { "Folders.pick": () => "/work/other" } },
  });
  await app.waitFor("label:Message");
  await app.type("label:Message", "/");
  await app.waitFor("text:Release first creation");
  await app.press("label:Workspace");
  await app.press("text:Choose folder…");
  await app.waitFor("text:/second");
  await app.press("text:Release first creation");
  await app.waitUntil(() => !app.shows("text:Release first creation"));
  expect(app.value("label:Message")).toBe("/");
  expect(app.shows("text:/first")).toBe(false);
  await app.press("text:/second");
  await app.submit("label:Message");
  await app.waitFor("text:Received on new-3");
});
