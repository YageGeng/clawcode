import { expect, open, test } from "nsh/test";

import { DESKTOP } from "../support/desktop";

/** Opens Session "s1" and switches it to its trajectory. */
async function openTrajectory() {
  const app = await open("../entries/session-trajectory.tsx", { ...DESKTOP, width: 1400, height: 900 });
  await app.waitFor("text:Done");
  await app.press("text:Trajectory");
  await app.waitFor("text:Run 2");
  return app;
}

test("the_trajectory_heads_runs_and_sums_the_loaded_records", async () => {
  const app = await openTrajectory();
  expect(app.shows("text:Run 1")).toBe(true);
  expect(app.shows("text:2 runs · 4 requests · 4.3k tok · cache hit 86%")).toBe(true);
  expect(app.shows("text:#2 · m · first token 100ms")).toBe(true);
  expect(app.shows("text:changed")).toBe(true);
  // A Run's total sits with its title, not under the Input column.
  expect(app.shows("text:2.2k tok")).toBe(true);
  expect(app.shows("text:Initial system prompt")).toBe(true);
  // The conversation is one press away and kept as it was.
  await app.press("text:Conversation");
  await app.waitFor("text:Fix it");
  expect(app.shows("text:Run 1")).toBe(false);
});

test("tool_calls_fold_per_request_and_runs_fold", async () => {
  const app = await openTrajectory();
  await app.press("text:2 tool calls >> nth=0");
  await app.waitFor("text:read a.rs → ok");
  await app.press("text:Run 1");
  await app.waitUntil(() => !app.texts().includes("read a.rs → ok"));
  await app.press("text:Group tool calls");
  await app.waitFor("text:exec_command {} → boom");
});

test("selecting_a_record_details_its_request", async () => {
  const app = await openTrajectory();
  await app.press("text:Done >> nth=1");
  await app.waitFor("text:Request #4 · m");
  expect(app.shows("text:Completed")).toBe(true);
  await app.press("text:Usage");
  await app.waitFor("text:This request");
  await app.press("text:System prompt");
  await app.waitFor("text:You are a careful coding agent.");
  await app.press("text:Tools");
  await app.waitFor("label:read");
  await app.press("label:Previous record");
  await app.waitFor("text:exec_command");
});

test("down_steps_past_a_requests_system_row", async () => {
  const app = await openTrajectory();
  await app.press("text:#2 · m · first token 100ms");
  await app.waitFor("text:Request #2 · m");
  // Request #2 changed its tools, so a system row repeats it right below; Down must not stop there.
  for (let i = 0; i < 3; i++) await app.keyboard.press("ArrowDown");
  await app.waitFor("text:Request #3 · m");
});

test("next_record_steps_past_the_output_shown_with_its_request", async () => {
  const app = await openTrajectory();
  await app.press("text:#2 · m · first token 100ms");
  await app.waitFor("text:Request #2 · m");
  // The request and its output are one view: Next moves on to the following Run's prompt.
  await app.press("label:Next record");
  await app.waitUntil(() => !app.shows("text:Request #2 · m"));
  await app.press("label:Previous record");
  await app.waitFor("text:Request #2 · m");
});

test("a_host_without_history_says_so", async () => {
  const app = await open("../entries/session-trajectory.tsx", { ...DESKTOP, width: 1400, height: 900 });
  await app.waitFor("text:No history");
  await app.press("text:No history >> nth=0");
  await app.waitFor("text:Trajectory");
  await app.press("text:Trajectory");
  await app.waitFor("text:The host keeps no session history, so the trajectory can't be shown.");
});
