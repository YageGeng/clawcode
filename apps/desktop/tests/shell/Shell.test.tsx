import { expect, open, test } from "nsh/test";

import { DESKTOP } from "../support/desktop";

/** Opens the shell entry at a desktop size; `modules` stands in for native exports. */
function openShell(modules?: Record<string, Record<string, unknown>>) {
  return open("../entries/shell.tsx", {
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

test("a_failed_first_attachment_shows_how_to_reload_history_from_the_selected_sidebar_row", async () => {
  const app = await openShell();
  await app.press("text:Second >> nth=0");
  await app.waitFor("text:Could not load the active branch of Second");
  await app.waitFor("text:Select the session again to reload its history.");
  expect(app.shows("text:Ready to proceed?")).toBe(false);
  await app.press("text:Second >> nth=0");
  await app.waitFor("text:Earlier request");
  await app.waitFor("text:Ready to proceed?");
});

test("an_offline_retry_keeps_the_open_transcript_and_unsent_draft_on_screen", async () => {
  const app = await open("../entries/reconnecting.tsx", DESKTOP);
  await app.waitFor("text:Conversation A");
  await app.press("text:Conversation A >> nth=0");
  await app.waitFor("text:Saved transcript");
  await app.type("label:Message", "Unsent draft");
  await app.press("text:Begin retry");
  await app.waitFor("text:Reconnecting…");
  await app.waitFor("text:Saved transcript");
  expect(app.value("label:Message")).toBe("Unsent draft");
  expect(app.enabled("label:Send")).toBe(false);
  await app.press("text:Fail retry");
  await app.waitFor("text:Retry");
  await app.waitFor("text:Saved transcript");
  expect(app.value("label:Message")).toBe("Unsent draft");
});

test("picking_a_folder_selects_it_for_the_new_session", async () => {
  const app = await openShell({ "clawcode-backend": { "Folders.pick": () => "/work/new" } });
  await app.waitFor("text:What should we build in");
  await app.press("label:Workspace");
  await app.press("text:Choose folder…");
  await app.waitFor("text:new");
  expect(app.texts()).toContain("new");
});

test("remote_workspace_selection_uses_the_remote_path_and_stays_with_its_host", async () => {
  const app = await openShell();
  await app.waitFor("text:alpha");
  await app.press("text:This computer >> nth=0");
  await app.press("text:Workstation");
  await app.press("label:Workspace");
  await app.press("text:Enter remote path…");
  await app.type("label:Remote workspace path", "/srv/project");
  await app.press("text:Use workspace");
  await app.waitFor("text:project");
  await app.press("text:Workstation >> nth=0");
  await app.press("text:This computer");
  await app.waitFor("text:alpha");
  expect(app.count("text:project")).toBe(0);
  await app.press("text:This computer >> nth=0");
  await app.press("text:Workstation");
  await app.waitFor("text:project");
});

test("configuring_a_new_session_keeps_the_draft_and_sends_it_without_creating_another_session", async () => {
  const app = await openShell();
  await app.waitFor("text:What should we build in");
  await app.type("label:Message", "Keep this first prompt");
  await app.press("label:Session settings");
  await app.waitFor("text:This session offers no model or thinking settings.");
  expect(app.value("label:Message")).toBe("Keep this first prompt");
  await app.keyboard.press("escape");
  await app.press("label:Send");
  await app.waitFor("text:Done.");
  await app.waitFor("text:Keep this first prompt");
  expect(app.count("text:new-4")).toBeGreaterThan(0);
  expect(app.count("text:new-5")).toBe(0);
});

test("switching_workspaces_after_configuration_sends_the_draft_to_the_new_workspace", async () => {
  const app = await open("../entries/session.tsx", {
    ...DESKTOP, modules: { "clawcode-backend": { "Folders.pick": () => "/work/other" } },
  });
  await app.waitFor("text:What should we build in");
  await app.type("label:Message", "Use the selected workspace");
  await app.press("label:Session settings");
  await app.waitFor("text:This session offers no model or thinking settings.");
  await app.keyboard.press("escape");
  await app.press("label:Workspace");
  await app.press("text:Choose folder…");
  await app.waitFor("text:other");
  await app.press("label:Send");
  await app.waitFor("text:Echo Use the selected workspace from /work/other");
});

test("new_session_discards_the_prepared_session_policy_and_starts_a_clean_draft", async () => {
  const app = await open("../entries/draft-navigation.tsx", DESKTOP);
  await app.waitFor("label:Message");
  await app.type("label:Message", "/");
  await app.press("text:Release first creation");
  await app.waitFor("text:/first");
  await app.press("label:File approvals");
  await app.waitFor("text:Allow external writes");
  expect(app.accessibility("role:menuitemcheckbox >> nth=1")?.checked).toBe("true");
  await app.keyboard.press("escape");
  await app.press("text:New session");
  expect(app.value("label:Message")).toBe("");
  await app.type("label:Message", "/");
  await app.waitFor("text:/second");
  await app.press("label:File approvals");
  await app.waitFor("text:Host default");
  expect(app.accessibility("role:menuitemcheckbox >> nth=0")?.checked).toBe("true");
  await app.keyboard.press("escape");
  await app.press("text:/second");
  await app.submit("label:Message");
  await app.waitFor("text:Received on new-3");
});

test("new_session_keeps_the_blank_page_when_an_earlier_first_send_finishes", async () => {
  const app = await open("../entries/draft-navigation.tsx", DESKTOP);
  await app.waitFor("label:Message");
  await app.type("label:Message", "First");
  await app.submit("label:Message");
  await app.waitFor("text:Release first creation");
  await app.press("text:New session");
  await app.type("label:Message", "Second");
  await app.press("text:Release first creation");
  await app.waitUntil(() => !app.shows("text:Release first creation"));
  await app.waitFor("text:What should we build in");
  expect(app.value("label:Message")).toBe("Second");
  await app.submit("label:Message");
  await app.waitFor("text:Received on new-3");
});

test("a_slow_first_send_stays_in_the_newly_selected_workspace_without_an_edit", async () => {
  const app = await open("../entries/draft-navigation.tsx", {
    ...DESKTOP, modules: { "clawcode-backend": { "Folders.pick": () => "/work/beta" } },
  });
  await app.waitFor("label:Message");
  await app.type("label:Message", "Send in alpha");
  await app.submit("label:Message");
  await app.waitFor("text:Release first creation");
  await app.press("label:Workspace");
  await app.press("text:Choose folder…");
  await app.waitFor("text:beta");
  await app.press("text:Release first creation");
  await app.waitUntil(() => app.value("label:Message") === "");
  await app.waitFor("text:What should we build in");
  await app.waitFor("text:beta");
  await app.press("text:new-2 >> nth=0");
  await app.waitFor("text:Received on new-2");
});

test("retrying_a_first_send_after_attachment_fails_uses_the_confirmed_session", async () => {
  const app = await open("../entries/draft-navigation.tsx", DESKTOP);
  await app.waitFor("label:Message");
  await app.type("label:Message", "Keep the pending prompt");
  await app.submit("label:Message");
  await app.press("text:Fail first attachment");
  await app.waitFor("text:Could not start the session");
  await app.waitFor("text:ACP -32603: First attachment failed");
  expect(app.value("label:Message")).toBe("Keep the pending prompt");
  await app.submit("label:Message");
  await app.waitFor("text:Received on new-2");
  await app.waitFor("text:Keep the pending prompt");
  expect(app.shows("text:new-3")).toBe(false);
});

test("editing_during_a_slow_first_send_keeps_the_edited_draft_in_the_same_workspace", async () => {
  const app = await open("../entries/draft-navigation.tsx", DESKTOP);
  await app.waitFor("label:Message");
  await app.type("label:Message", "First");
  await app.submit("label:Message");
  await app.waitFor("text:Release first creation");
  await app.clear("label:Message");
  await app.type("label:Message", "Follow-up");
  await app.press("text:Release first creation");
  await app.waitUntil(() => app.enabled("label:Send"));
  await app.waitFor("text:What should we build in");
  expect(app.value("label:Message")).toBe("Follow-up");
  await app.submit("label:Message");
  await app.waitFor("text:Received on new-3");
  await app.waitFor("text:Follow-up");
});

test("new_session_ignores_late_preparation_on_the_same_blank_page", async () => {
  const app = await open("../entries/draft-navigation.tsx", DESKTOP);
  await app.waitFor("label:Message");
  await app.press("label:File approvals");
  await app.waitFor("text:Release first creation");
  await app.press("text:New session");
  await app.press("text:Release first creation");
  await app.waitUntil(() => !app.shows("text:Release first creation"));
  expect(app.shows("text:Allow external writes")).toBe(false);
  await app.type("label:Message", "/");
  await app.waitFor("text:/second");
});
