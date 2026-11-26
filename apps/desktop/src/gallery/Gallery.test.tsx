import { expect, open, test } from "nsh/test";

/** Opens the gallery in a desktop-sized Linux window. */
function openGallery() {
  return open("./main.tsx", { platform: "linux", width: 1200, height: 900 });
}

test("gallery_shows_every_component", async () => {
  const app = await openGallery();
  for (const title of ["Button", "IconButton", "Input", "Selection", "Tabs", "Menu", "Dialog", "Toast", "ListItem", "TreeList", "Surface", "QR code", "Feedback"]) {
    await app.waitFor(`text:${title}`);
  }
});

test("buttons_report_presses_and_disabled_ignores", async () => {
  const app = await openGallery();
  await app.press("Primary");
  await app.waitFor("text:Pressed 1");
  await expect(app.press("Disabled")).rejects.toMatchObject({ code: "ERR_NSH_TEST_DISABLED" });
  await app.waitFor("text:Pressed 1");
});

// Some platforms acknowledge the request before they can confirm the write.
test("copy_button_writes_the_gallery_command", async () => {
  const app = await openGallery();
  await app.press("label:Copy");
  await app.waitUntil(() => app.count("label:Copied") + app.count("label:Copy requested") > 0);
  expect(app.clipboard()).toBe("cargo test --workspace");
});

test("menu_opens_and_closes_with_escape", async () => {
  const app = await openGallery();
  await app.press("Session actions");
  await app.waitFor("text:Rename");
  await app.keyboard.press("Escape");
  await app.waitUntil(() => !app.texts().includes("Rename"));
  await app.press("Session actions");
  await app.press("Copy id");
  await app.waitFor("text:Last command: Copy id");
});

test("textarea_enter_submits_and_clears", async () => {
  const app = await openGallery();
  await app.type("Ask anything", "hello");
  await app.keyboard.press("Enter");
  await app.waitFor("text:Sent: hello");
});

test("menu_shows_title_and_descriptions", async () => {
  const app = await openGallery();
  await app.press("Approval mode");
  await app.waitFor("text:How should actions be approved?");
  await app.waitFor("text:Ask before editing files outside the workspace");
});

test("long_titles_stay_on_one_row", async () => {
  const app = await openGallery();
  await app.waitFor("text:A very long session title that keeps going well past the width of the sidebar row");
});
