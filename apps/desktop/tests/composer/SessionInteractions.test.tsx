import { expect, open, test } from "nsh/test";

import { DESKTOP } from "../support/desktop";

test("another_client_reserving_an_earlier_question_keeps_the_current_answer_target", async () => {
  const app = await open("../entries/session-interactions.tsx", DESKTOP);
  await app.press("label:Next question");
  await app.type("label:Your answer", "review notes");
  await app.type("label:Message", "Reserve first question");
  await app.submit("label:Message");
  await app.waitFor("text:Question 1 of 2");
  await app.waitFor("text:Add a note?");
  expect(app.value("label:Your answer")).toBe("review notes");
  await app.press("label:Send answer");
  await app.waitFor("text:Approve this plan?");
  await app.press("label:Approve and execute");
  await app.waitFor("text:review notes");
});

test("an_explicitly_open_plan_stays_open_as_task_progress_arrives", async () => {
  const app = await open("../entries/session-interactions.tsx", DESKTOP);
  await app.press("label:Plan · 0 of 1 tasks done");
  await app.waitFor("text:Build the index");
  await app.type("label:Message", "Update task progress");
  await app.submit("label:Message");
  await app.waitFor("label:Plan · 1 of 1 tasks done");
  await app.waitFor("text:Build the index");
  await app.waitFor("text:Run checks");
});

test("question_navigation_keeps_free_text_and_answers_without_changing_the_main_composer", async () => {
  const app = await open("../entries/session-interactions.tsx", DESKTOP);
  await app.waitFor("label:Next question");
  await app.waitFor("label:Awaiting your answer");
  await app.type("label:Message", "my prompt");
  await app.press("label:Next question");
  await app.type("label:Your answer", "later");
  await app.press("label:Previous question");
  await app.press("label:Next question");
  expect(app.value("label:Your answer")).toBe("later");
  await app.press("label:Previous question");
  await app.press("text:Windows");
  await app.press("label:Send answer");
  await app.waitFor("text:Approve this plan?");
  expect(app.texts()).not.toContain("Test both environments");
  await app.press("label:Proposed plan");
  await app.waitFor("text:Test both environments");
  await app.press("label:Approve and execute");
  await app.waitFor("text:Question 1 of 2");
  // The receipt shows the selected options in the final reply.
  await app.waitFor("text:Linux");
  await app.waitFor("text:Windows");
  expect(app.value("label:Message")).toBe("my prompt");
  await app.press("label:Skip all");
  await app.waitUntil(() => !app.texts().includes("Skip all"));
  await app.press("label:Plan · 0 of 1 tasks done");
  await app.waitFor("text:Build the index");
  await app.waitFor("text:Run checks");
});
