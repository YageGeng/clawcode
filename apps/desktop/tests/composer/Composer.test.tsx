import { expect, open, test } from "nsh/test";

import { DESKTOP } from "../support/desktop";

/** Opens the composer entry at a desktop size; `modules` stands in for native exports. */
function openEntry(modules?: Record<string, Record<string, unknown>>) {
  return open("../entries/session.tsx", {
    ...DESKTOP,
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

test("external_write_policy_can_be_tightened_while_the_prompt_is_running", async () => {
  const app = await openEntry();
  await send(app, "Slow", "keep working");
  await app.waitFor("label:Stop");
  expect(app.enabled("label:Session mode")).toBe(false);
  await app.press("label:File approvals");
  await app.press("text:Ask every time");
  await app.press("label:File approvals");
  await app.waitUntil(() => app.accessibility("role:menuitemcheckbox >> nth=0")?.checked === "true");
  await app.keyboard.press("Escape");
  await app.press("label:Stop");
  await app.waitFor("text:Stopped.");
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
  // An open Session shows the lost backend as an offline status in its header; the reason is its tooltip.
  await app.waitFor("text:Offline");
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

test("opening_and_closing_settings_leaves_a_pending_request_unanswered", async () => {
  const app = await openEntry();
  await send(app, "Asks", "hello");
  await app.waitFor("text:Run cargo test?");
  // Settings open in a window of their own; closing it must not answer the request in the main window.
  await app.press("text:Settings");
  await app.waitFor("label:Open workspaces in");
  await app.window("settings").pressClose();
  await app.waitUntil(() => !app.shows("label:Open workspaces in"));
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

test("session_settings_keep_the_last_confirmed_choice_after_failure", async () => {
  const app = await openEntry();
  await app.press("text:Echo >> nth=0");
  await app.waitFor("label:Session mode");
  await app.type("label:Message", "keep this draft");
  await app.press("label:Session settings");
  await app.waitFor("label:Thinking level: Low");
  await app.press("label:Model");
  await app.press("text:Model B");
  await app.waitFor("text:Could not change the session setting. Try again.");
  // The old server value remains visible and the failed change leaves the text available for editing.
  await app.waitFor("text:Model A");
  expect(app.value("label:Message")).toBe("keep this draft");
  await app.keyboard.press("Escape");
  await app.press("label:Session mode");
  await app.press("text:Plan");
  await app.waitFor("text:Default");
});

test("offline_session_settings_show_cached_values_and_disable_changes", async () => {
  const app = await openEntry();
  await send(app, "Crashes", "disconnect");
  // An open Session shows the lost backend as an offline status in its header; the reason is its tooltip.
  await app.waitFor("text:Offline");
  expect(app.enabled("label:Session mode")).toBe(false);
  await app.press("label:Session settings");
  await app.waitFor("label:Thinking level: Low");
  expect(app.enabled("label:Model")).toBe(false);
  await app.waitFor("text:Model A");
  await app.keyboard.press("escape");
  await app.press("label:File approvals");
  await app.waitFor("text:Allow external writes");
  expect(app.accessibility("role:menuitemcheckbox >> nth=1")?.checked).toBe("true");
  expect(app.enabled("text:Allow external writes")).toBe(false);
  expect(app.enabled("text:Ask every time")).toBe(false);
});

test("thinking_level_keyboard_step_reaches_the_host", async () => {
  const app = await openEntry();
  await app.press("text:Echo >> nth=0");
  await app.press("label:Session settings");
  await app.waitFor("label:Thinking level: Low");
  await app.press("label:Thinking level: Low");
  await app.keyboard.press("right");
  await app.waitFor("text:Could not change the session setting. Try again.", { timeout: 800 });
});

// A valid 1 × 1 PNG whose exact bytes can be selected in the native picker's headless driver.
const imageFile = {
  name: "pixel.png", type: "image/png",
  data: Uint8Array.from(atob("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+yLlEAAAAASUVORK5CYII="), (c) => c.charCodeAt(0)),
};

test("attachments_preview_remove_and_send_an_image_only_prompt", async () => {
  const app = await openEntry();
  await app.press("text:Echo >> nth=0");
  app.onPick(() => [imageFile]);
  await app.press("label:Attach images");
  await app.waitFor("label:pixel.png");
  expect(app.enabled("label:Send")).toBe(true);
  await app.press("label:Remove pixel.png");
  expect(app.enabled("label:Send")).toBe(false);
  await app.press("label:Attach images");
  await app.waitFor("label:pixel.png");
  await app.press("label:Send");
  await app.waitFor("text:1 image");
  expect(app.shows("label:Remove pixel.png")).toBe(false);
});

test("attachments_return_with_the_text_on_a_failed_prompt", async () => {
  const app = await openEntry();
  await app.press("text:Broken >> nth=0");
  app.onPick(() => [imageFile]);
  await app.press("label:Attach images");
  await app.waitFor("label:pixel.png");
  await app.type("label:Message", "look here");
  await app.submit("label:Message");
  await app.waitFor("text:The message was not sent");
  await app.waitFor("label:Remove pixel.png");
  expect(app.value("label:Message")).toBe("look here");
});

test("attachments_reject_unsupported_files_and_keep_the_draft", async () => {
  const app = await openEntry();
  await app.press("text:Echo >> nth=0");
  app.onPick(() => [{ name: "notes.txt", type: "text/plain", data: new Uint8Array([65]) }]);
  await app.type("label:Message", "keep me");
  await app.press("label:Attach images");
  await app.waitFor("text:Choose a non-empty PNG, JPEG, GIF or WebP image.");
  expect(app.value("label:Message")).toBe("keep me");
});

test("an_overflowing_native_picker_selection_keeps_only_the_previous_attachments", async () => {
  const app = await openEntry();
  await app.press("text:Echo >> nth=0");
  app.onPick(() => [imageFile]);
  await app.press("label:Attach images");
  await app.waitFor("label:pixel.png");
  // Apply the native result cap even on platforms whose headless handler returns every selection.
  app.onPick((request) => Array.from({ length: 6 }, (_, i) => ({ ...imageFile, name: `extra-${i}.png` })).slice(0, request.limit));
  await app.press("label:Attach images");
  await app.waitFor("text:Attach up to 5 images per message.");
  await app.waitFor("label:pixel.png");
  expect(app.shows("label:extra-0.png")).toBe(false);
});

test("a_new_draft_names_the_hosts_default_model", async () => {
  const app = await openEntry();
  await app.waitFor("label:Session settings");
  expect(app.shows("text:Model A")).toBe(true);
  expect(app.shows("text:Session settings")).toBe(false);
});
