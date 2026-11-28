import { expect, open, test } from "nsh/test";

import { DESKTOP } from "../support/desktop";

/** Opens Session "s1" with two finished turns. */
async function openTurns() {
  const app = await open("../entries/session-edit.tsx", DESKTOP);
  await app.waitFor("text:Second answer");
  return app;
}

test("a_user_message_copies_its_text", async () => {
  const app = await openTurns();
  // Transcript order: the first prompt's Copy comes first.
  await app.press("label:Copy >> nth=0");
  await app.waitUntil(() => app.clipboard() === "First question");
});

test("cancelling_an_edit_restores_the_draft", async () => {
  const app = await openTurns();
  await app.type("label:Message", "half written");
  await app.press("label:Edit and ask again >> nth=1");
  await app.waitFor("text:Editing an earlier message");
  expect(app.value("label:Message")).toBe("Second question");
  // A second edit replaces the text, yet cancelling still returns the original draft.
  await app.press("label:Edit and ask again >> nth=0");
  await app.waitUntil(() => app.value("label:Message") === "First question");
  await app.press("label:Cancel");
  await app.waitUntil(() => app.value("label:Message") === "half written");
  expect(app.shows("text:Editing an earlier message")).toBe(false);
});

test("sending_an_edit_asks_again_on_a_new_branch", async () => {
  const app = await openTurns();
  await app.press("label:Edit and ask again >> nth=1");
  await app.waitUntil(() => app.value("label:Message") === "Second question");
  await app.clear("label:Message");
  await app.type("label:Message", "Changed question");
  await app.submit("label:Message");
  await app.waitFor("text:Done.");
  const texts = app.texts();
  expect(texts).toContain("First answer");
  expect(texts).toContain("Changed question");
  expect(texts).not.toContain("Second question");
  expect(texts).not.toContain("Second answer");
  expect(app.shows("text:Editing an earlier message")).toBe(false);
});

test("a_sent_edit_gives_back_the_draft_it_replaced", async () => {
  const app = await openTurns();
  await app.type("label:Message", "half written");
  await app.press("label:Edit and ask again >> nth=1");
  await app.waitUntil(() => app.value("label:Message") === "Second question");
  await app.submit("label:Message");
  await app.waitFor("text:Done.");
  await app.waitUntil(() => app.value("label:Message") === "half written");
});

test("escape_cancels_an_edit_immediately_and_restores_the_draft", async () => {
  const app = await openTurns();
  await app.type("label:Message", "unsent note");
  await app.press("label:Edit and ask again >> nth=1");
  await app.waitFor("text:Editing an earlier message");
  expect(app.value("label:Message")).toBe("Second question");
  await app.keyboard.press("Escape");
  await app.waitUntil(() => app.value("label:Message") === "unsent note");
  expect(app.shows("text:Editing an earlier message")).toBe(false);
});

test("escape_in_the_settings_window_keeps_the_edit", async () => {
  const app = await openTurns();
  await app.press("label:Edit and ask again >> nth=1");
  await app.waitFor("text:Editing an earlier message");
  await app.press("text:Settings");
  await app.waitUntil(() => app.window("settings").state()?.focused === true);
  await app.keyboard.press("Escape");
  expect(app.shows("text:Editing an earlier message")).toBe(true);
  expect(app.value("label:Message")).toBe("Second question");
});

test("forking_a_reply_opens_the_copy_up_to_it", async () => {
  const app = await openTurns();
  await app.press("label:Fork into a new session >> nth=0");
  await app.waitFor("text:Fix the build (fork) >> nth=0");
  await app.waitUntil(() => !app.texts().includes("Second answer"));
  expect(app.texts()).toContain("First answer");
});
