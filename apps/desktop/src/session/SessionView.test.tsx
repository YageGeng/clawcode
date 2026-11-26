import { expect, open, test } from "nsh/test";

import { DESKTOP } from "../../tests/support/desktop";

/** Opens a session test entry at a desktop size. */
function openEntry(name: string, options: Record<string, unknown> = {}) {
  return open(`../../tests/entries/${name}.tsx`, { ...DESKTOP, ...options });
}

test("ended_turn_folds_work_under_worked_for", async () => {
  const app = await openEntry("session-ended");
  await app.waitFor("text:Fixed it.");
  await app.waitFor("text:Worked for 4s");
  expect(app.texts()).not.toContain("cargo build");
  await app.press("text:Worked for 4s");
  await app.waitFor("text:Let me look.");
  await app.waitFor("text:Ran commands");
  await app.press("text:Ran commands");
  await app.waitFor("text:cargo build");
  await app.waitFor("text:cargo test");
});

test("running_turn_shows_work_expanded", async () => {
  const app = await openEntry("session-running");
  await app.waitFor("text:Working");
  await app.waitFor("text:Looking at the logs");
});

test("file_changes_show_a_card", async () => {
  const app = await openEntry("session-changes");
  await app.waitFor("text:Edited a.rs");
  await app.waitFor("text:+3");
});

test("follows_the_bottom_only_when_at_bottom", async () => {
  const app = await openEntry("session-long", { clock: {} });
  await app.waitFor("text:Answer 60");
  await app.wait(1200);
  await app.waitFor("text:Tail one");
  await app.scroll("text:Tail one", -2000);
  await app.waitFor("label:Jump to bottom");
  await app.wait(2200);
  expect(app.texts()).not.toContain("Tail two");
  await app.press("label:Jump to bottom");
  await app.waitFor("text:Tail two");
});
