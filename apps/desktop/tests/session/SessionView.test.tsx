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

test("streaming_reply_stays_visible_while_work_is_folded", async () => {
  const app = await openEntry("session-running", { clock: {} });
  await app.waitFor("label:Working");
  expect(app.texts()).not.toContain("Check the last error.");
  await app.waitFor("text:Looking at the logs");
  const streaming = ["Current file", "Build output", "Looking at the logs"];
  expect(app.texts().filter((value) => streaming.includes(value))).toEqual(streaming);
  expect(app.shows("label:Copy")).toBe(false);
  expect(app.shows("label:Assistant reply")).toBe(false);
  await app.press("label:Working");
  await app.press("label:Thinking");
  await app.waitFor("text:Check the last error.");
  await app.press("label:Working");
  expect(app.texts()).not.toContain("Check the last error.");
  await app.waitFor("text:Looking at the logs");
  await app.wait(10000);
  await app.waitFor("label:Copy");
  const final = ["Final file", "Final output", "Build passed."];
  expect(app.texts().filter((value) => final.includes(value))).toEqual(final);
  expect(app.texts()).not.toContain("Current file");
  await app.press("label:Copy");
  await app.waitUntil(() => app.clipboard() === "Final fileBuild passed.");
});

test("file_changes_and_native_diff_start_closed", async () => {
  const app = await openEntry("session-changes");
  await app.waitFor("label:Changed files (1)");
  expect(app.texts()).not.toContain("Edited a.rs");
  await app.press("label:Changed files (1)");
  await app.waitFor("text:Edited a.rs");
  await app.waitFor("text:+3");
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
