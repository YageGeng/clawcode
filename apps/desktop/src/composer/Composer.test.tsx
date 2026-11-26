import { expect, open, test } from "nsh/test";

/** Opens the composer entry at a desktop size; `modules` stands in for native exports. */
function openEntry(modules?: Record<string, Record<string, unknown>>) {
  return open("../../tests/entries/session.tsx", {
    platform: "linux",
    width: 1200,
    height: 800,
    ...(modules === undefined ? {} : { modules: modules as never }),
  });
}

/** Opens the Session titled `title` from the sidebar and sends `text`. */
async function send(app: Awaited<ReturnType<typeof openEntry>>, title: string, text: string) {
  await app.waitFor(`text:${title}`);
  await app.press(`text:${title} >> nth=0`);
  await app.waitFor("label:Send");
  await app.type("label:Message", text);
  await app.submit("label:Message");
}

test("enter_sends_and_the_button_becomes_stop", async () => {
  const app = await openEntry();
  await app.waitFor("text:Echo");
  await app.press("text:Echo >> nth=0");
  await app.waitFor("label:Send");
  expect(app.enabled("label:Send")).toBe(false);
  await app.type("label:Message", "hello");
  await app.submit("label:Message");
  await app.waitFor("label:Stop");
  await app.waitFor("text:Echo hello from /work/alpha");
  await app.waitFor("label:Send");
  expect(app.value("label:Message")).toBe("");
});

test("stop_cancels_the_turn", async () => {
  const app = await openEntry();
  await send(app, "Slow", "hello");
  await app.waitFor("label:Stop");
  await app.press("label:Stop");
  await app.waitFor("text:Stopped.");
  await app.waitFor("label:Send");
});

test("failed_prompt_puts_the_text_back", async () => {
  const app = await openEntry();
  await send(app, "Broken", "hello");
  await app.waitFor("text:The message was not sent");
  await app.waitUntil(() => app.value("label:Message") === "hello");
});

test("a_lost_backend_keeps_the_session_and_returns_the_text", async () => {
  const app = await openEntry();
  await send(app, "Crashes", "hello");
  // The Session stays readable under the reason, the sidebar keeps its rows, and what was sent is back in the field.
  await app.waitFor("text:Backend stopped: crashed");
  await app.waitFor("text:hello");
  await app.waitFor("text:Echo");
  await app.waitUntil(() => app.value("label:Message") === "hello");
  await app.waitFor("label:Send");
});

test("permission_card_answers_and_disappears", async () => {
  const app = await openEntry();
  await send(app, "Asks", "hello");
  await app.waitFor("text:Run cargo test?");
  await app.press("text:Allow once");
  await app.waitFor("text:Chose allow-once");
  await app.waitUntil(() => !app.texts().includes("Run cargo test?"));
});

test("a_session_waiting_for_approval_says_so_in_the_sidebar", async () => {
  const app = await openEntry();
  await send(app, "Asks", "hello");
  await app.waitFor("text:Run cargo test?");
  // The request comes mid-turn, so the row is running too: waiting on the user must win over the spinner.
  await app.waitFor("label:Needs your approval");
});

test("escape_rejects_the_permission", async () => {
  const app = await openEntry();
  await send(app, "Asks", "hello");
  await app.waitFor("text:Run cargo test?");
  await app.keyboard.press("escape");
  await app.waitFor("text:Chose cancelled");
  await app.waitUntil(() => !app.texts().includes("Run cargo test?"));
});

test("switching_sessions_starts_a_clean_draft", async () => {
  const app = await openEntry();
  await app.waitFor("text:Echo");
  await app.press("text:Echo >> nth=0");
  await app.waitFor("label:Send");
  await app.type("label:Message", "meant for Echo");
  // Another Session's composer must not offer Echo's draft: Enter there would send it into the wrong workspace.
  await app.press("text:Slow >> nth=0");
  await app.waitFor("text:Slow >> nth=2");
  expect(app.value("label:Message")).toBe("");
});

test("escape_closes_an_open_dialog_before_it_rejects", async () => {
  const app = await openEntry();
  await send(app, "Asks", "hello");
  await app.waitFor("text:Run cargo test?");
  await app.press("text:Settings");
  await app.waitFor("text:Appearance");
  // Escape belongs to the topmost surface: it closes Settings and leaves the request unanswered.
  await app.keyboard.press("escape");
  await app.waitUntil(() => !app.texts().includes("Appearance"));
  await app.wait(300);
  expect(app.texts().includes("Run cargo test?")).toBe(true);
  expect(app.texts().some((text) => text.startsWith("Chose"))).toBe(false);
});

test("new_session_in_a_picked_folder", async () => {
  const app = await openEntry({ "clawcode-backend": { "Folders.pick": () => "/work/new" } });
  await app.waitFor("text:What should we build in");
  await app.press("label:Workspace");
  await app.press("text:Choose folder…");
  await app.waitFor("text:new");
  await app.type("label:Message", "hi");
  await app.submit("label:Message");
  await app.waitFor("text:Echo hi from /work/new");
});
