import { expect, open, test } from "nsh/test";

import { DESKTOP } from "../../tests/support/desktop";

/** Opens the shell entry at a desktop size; `modules` stands in for native exports. */
function openShell(modules?: Record<string, Record<string, unknown>>) {
  return open("../../tests/entries/shell.tsx", {
    ...DESKTOP,
    ...(modules === undefined ? {} : { modules: modules as never }),
  });
}

test("sidebar_groups_sessions_by_workspace", async () => {
  const app = await openShell();
  await app.waitFor("text:alpha");
  await app.waitFor("text:beta");
  await app.waitFor("text:Third");
  await app.waitFor("text:First");
});

test("opening_a_session_selects_it_and_back_returns", async () => {
  const app = await openShell();
  await app.waitFor("text:First");
  await app.waitFor("text:Third");
  // The open Session's title also shows in the main area's header: one more match than the sidebar's.
  const first = app.count("text:First");
  const third = app.count("text:Third");
  await app.press("text:First >> nth=0");
  await app.waitUntil(() => app.count("text:First") === first + 1);
  await app.press("text:Third >> nth=0");
  await app.waitUntil(() => app.count("text:Third") === third + 1 && app.count("text:First") === first);
  await app.press("label:Back");
  await app.waitUntil(() => app.count("text:First") === first + 1);
});

test("running_session_shows_a_spinner", async () => {
  const app = await openShell();
  await app.waitFor("label:Running");
});

test("picking_a_folder_selects_it_for_the_new_session", async () => {
  const app = await openShell({ "clawcode-backend": { "Folders.pick": () => "/work/new" } });
  await app.waitFor("text:What should we build in");
  await app.press("label:Workspace");
  await app.press("text:Choose folder…");
  await app.waitFor("text:new");
  expect(app.texts()).toContain("new");
});
