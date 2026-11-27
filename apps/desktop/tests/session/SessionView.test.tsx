import { expect, open, test } from "nsh/test";

import { DESKTOP } from "../support/desktop";

/** Opens a session test entry at a desktop size. */
function openEntry(name: string, options: Record<string, unknown> = {}) {
  return open(`../entries/${name}.tsx`, { ...DESKTOP, ...options });
}

test("mixed_reply_shell_and_question_choices_remain_visible_in_transcript_order", async () => {
  const app = await openEntry("session-decisions", { width: 1200, height: 1100 });
  await app.waitFor("text:Format confirmed.");
  const ordered = ["Before the file.", "Report file", "After the file.", "!pwd", "Which format?", "Plain text",
    "Add a chart?", "Skipped question", "Format confirmed."];
  expect(app.texts().filter((value) => ordered.includes(value))).toEqual(ordered);
  expect(app.shows("label:Copy >> nth=1")).toBe(true);
  await app.press("label:Copy >> nth=0");
  await app.waitUntil(() => app.clipboard() === "Before the file.After the file.");
});

test("work_discloses_each_thought_message_and_named_tool_independently", async () => {
  const app = await openEntry("session-ended");
  await app.waitFor("text:Fixed it.");
  expect(app.shows("label:Assistant reply")).toBe(false);
  await app.press("label:Worked for 7s");
  for (const label of ["Thinking", "Assistant message", "exec_command", "read", "mcp__custom__lookup"]) {
    await app.waitFor(`label:${label} >> nth=0`);
  }
  for (const body of ["Inspect the build log.", "Let me look.", "File contents from read", "Repository result"]) {
    expect(app.texts()).not.toContain(body);
  }
  await app.press("label:Thinking >> nth=0");
  await app.waitFor("text:Inspect the build log.");
  await app.press("label:Assistant message");
  await app.waitFor("text:Let me look.");
  const interim = ["Let me look.", "Build log", "Checking the command."];
  expect(app.texts().filter((value) => interim.includes(value))).toEqual(interim);
  expect(app.texts()).not.toContain("The build now passes.");
  await app.press("label:read");
  await app.waitFor("text:File contents from read");
  await app.press("label:read");
  await app.press("label:mcp__custom__lookup");
  await app.waitFor("text:Repository result");
  await app.press("label:Worked for 7s");
  expect(app.texts()).not.toContain("Repository result");
  await app.press("label:Worked for 7s");
  await app.waitFor("text:Repository result");
  expect(app.texts()).not.toContain("File contents from read");
  await app.press("label:Worked for 7s");
  await app.waitFor("text:Fixed it.");
});

test("running_work_stays_open_and_folds_when_the_turn_ends", async () => {
  const app = await openEntry("session-running", { clock: {} });
  await app.waitFor("label:Working");
  // While the turn runs, its work and thinking show without a press.
  await app.waitFor("text:Check the last error.");
  await app.waitFor("text:Looking at the logs");
  const streaming = ["Current file", "Build output", "Looking at the logs"];
  expect(app.texts().filter((value) => streaming.includes(value))).toEqual(streaming);
  expect(app.shows("label:Copy")).toBe(false);
  expect(app.shows("label:Assistant reply")).toBe(false);
  // The reader can still fold the live work, and open it again.
  await app.press("label:Working");
  expect(app.texts()).not.toContain("Check the last error.");
  await app.press("label:Working");
  await app.waitFor("text:Check the last error.");
  await app.waitFor("text:Looking at the logs");
  await app.wait(10000);
  await app.waitFor("label:Copy");
  const final = ["Final file", "Final output", "Build passed."];
  expect(app.texts().filter((value) => final.includes(value))).toEqual(final);
  expect(app.texts()).not.toContain("Current file");
  // Once the turn ends, its work folds away.
  expect(app.texts()).not.toContain("Check the last error.");
  await app.press("label:Copy");
  await app.waitUntil(() => app.clipboard() === "Final fileBuild passed.");
});

test("file_changes_and_native_diff_start_closed", async () => {
  const app = await openEntry("session-changes");
  await app.waitFor("label:1 file changed");
  expect(app.accessibility("label:1 file changed")?.expanded).toBe(false);
  expect(app.texts()).not.toContain("Edited src/a.rs");
  await app.press("label:1 file changed");
  await app.waitFor("text:Edited src/a.rs");
  expect(app.accessibility("label:1 file changed")?.expanded).toBe(true);
  await app.waitFor("text:+3");
  await app.press("label:View changes to src/a.rs");
  await app.waitFor("text:src/a.rs");
  await app.waitFor("text:const MAX_RETRIES: u8 = 8;");
  await app.press("label:Close workspace panel");
  expect(app.shows("text:src/a.rs")).toBe(false);
});

test("review_opens_the_folded_turn_and_keeps_the_edit_being_read", async () => {
  const app = await openEntry("session-changes", { height: 1000 });
  await app.waitFor("label:2 files changed >> text:+3");
  await app.waitFor("label:2 files changed >> text:−2");
  await app.press("label:Review >> nth=0");
  await app.waitFor("text:use first;");
  expect(app.shows("label:View changes to tests/a.rs")).toBe(false);
  await app.press("label:Next edit");
  await app.press("label:Review >> nth=0");
  await app.waitFor("text:use second;");
  await app.press("label:Review >> nth=1");
  await app.waitFor("text:const MAX_RETRIES: u8 = 8;");
  await app.press("label:Close workspace panel");
  expect(app.shows("text:Edited src/a.rs")).toBe(false);
});

test("the_inspector_follows_the_clicked_path_and_turn_and_keeps_each_recorded_edit", async () => {
  const app = await openEntry("session-changes", { height: 1000 });
  await app.press("label:2 files changed");
  await app.press("label:View changes to src/a.rs");
  await app.waitFor("text:Turn 1");
  await app.waitFor("text:use first;");
  await app.waitFor("text:Edit 1 of 2");
  await app.press("label:Next edit");
  await app.waitFor("text:use second;");
  await app.press("label:View changes to tests/a.rs");
  await app.waitFor("text:fn test_only() {}");
  expect(app.shows("label:Next edit")).toBe(false);
  await app.press("label:1 file changed");
  await app.press("label:View changes to src/a.rs >> nth=1");
  await app.waitFor("text:const MAX_RETRIES: u8 = 8;");
  await app.waitFor("text:Turn 2");
  await app.press("text:Settings");
  await app.waitFor("label:Close");
  await app.keyboard.press("Escape");
  await app.waitFor("label:Close workspace panel");
  expect(app.shows("label:Close")).toBe(false);
  expect(app.shows("text:use first;")).toBe(false);
});

test("live_tool_updates_preserve_the_selected_edit_its_counts_and_its_scroll_offset", async () => {
  const app = await openEntry("session-change-updates");
  await app.press("label:1 file changed");
  await app.press("label:View changes to review.ts");
  await app.waitFor("text:Edit 1 of 3");
  await app.waitFor("label:182 lines added, 1 removed");
  await app.waitFor("label:1 lines added, 1 removed");
  await app.waitFor("text:−");
  await app.press("label:Next edit");
  await app.waitFor("label:180 lines added, 0 removed");
  await app.scroll("text:12", 750);
  const scrolled = app.layout("text:42")!;
  expect(scrolled.clipped).toBe(false);

  // A duplicate full payload and a withdrawn earlier sibling both keep the reader on the same line.
  for (const action of ["resend", "first"]) {
    await app.type("label:Message", action);
    await app.submit("label:Message");
    await app.waitFor(`text:Applied ${action}`);
    expect(app.layout("text:42")?.y).toBe(scrolled.y);
  }
  await app.waitFor("text:Edit 1 of 2");
  await app.waitFor("label:180 lines added, 0 removed");

  // If the selected edit itself is withdrawn, the panel displays the first surviving edit's statistics.
  await app.type("label:Message", "second");
  await app.submit("label:Message");
  await app.waitFor("text:last");
  await app.waitFor("label:1 lines added, 0 removed >> nth=1");
  expect(app.shows("label:Next edit")).toBe(false);
});

test("closing_a_narrow_inspector_returns_to_the_expanded_card_and_unsent_draft", async () => {
  const app = await openEntry("session-changes", { width: 800 });
  await app.type("label:Message", "Keep this draft");
  await app.press("label:1 file changed");
  await app.press("label:View changes to src/a.rs");
  await app.waitFor("text:src/a.rs");
  await app.keyboard.press("Escape");
  await app.waitFor("label:View changes to src/a.rs");
  expect(app.value("label:Message")).toBe("Keep this draft");
});

test("follows_the_bottom_only_when_at_bottom", async () => {
  const app = await openEntry("session-long", { clock: {} });
  await app.waitFor("text:Question 60");
  await app.wait(1200);
  await app.waitFor("text:Question 61");
  await app.scroll("text:Question 61", -2000);
  await app.waitFor("label:Jump to bottom");
  await app.wait(2200);
  expect(app.texts()).not.toContain("Question 62");
  await app.press("label:Jump to bottom");
  await app.waitFor("text:Question 62");
});
