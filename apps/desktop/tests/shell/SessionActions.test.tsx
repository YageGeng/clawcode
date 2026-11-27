import { expect, open, test } from "nsh/test";

import { DESKTOP } from "../support/desktop";

test("a_session_row_pins_renames_and_deletes_from_its_menu", async () => {
  const app = await open("../entries/session-details.tsx", DESKTOP);
  await app.waitFor("text:Original plan");

  await app.press("label:Actions for Original plan >> nth=0");
  await app.press("text:Pin");
  await app.waitFor("text:Pinned");
  expect(app.count("text:Original plan")).toBeGreaterThan(1);

  await app.press("label:Actions for Original plan >> nth=0");
  await app.press("text:Rename");
  await app.clear("label:Session title");
  await app.type("label:Session title", "Plan v2");
  await app.submit("label:Session title");
  await app.waitFor("text:Plan v2");

  app.onDialog((dialog) => dialog.buttons.includes("Delete") ? "Delete" : null);
  await app.press("label:Actions for Plan v2 >> nth=0");
  await app.press("text:Delete");
  await app.waitUntil(() => !app.shows("text:Plan v2"));
  expect(app.shows("text:Pinned")).toBe(false);
});

test("after_a_delete_another_rows_menu_opens_once", async () => {
  const app = await open("../entries/session.tsx", DESKTOP);
  await app.waitFor("text:Echo");
  app.onDialog((dialog) => dialog.buttons.includes("Delete") ? "Delete" : null);
  await app.press("label:Actions for Slow >> nth=0");
  await app.press("text:Delete");
  await app.waitUntil(() => !app.shows("text:Slow"));
  await app.press("label:Actions for Broken >> nth=0");
  await app.waitFor("text:Rename");
  // Recycled list cells must not open a second, detached copy of the menu.
  expect(app.count("text:Rename")).toBe(1);
});
