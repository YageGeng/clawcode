import { expect, open, test } from "nsh/test";

import { DESKTOP } from "../support/desktop";

test("browsing_files_preserves_the_selected_edit_and_discards_late_previews", async () => {
  const app = await open("../entries/session-files.tsx", DESKTOP);
  await app.press("label:1 file changed");
  await app.press("label:View changes to src/fast.ts");
  await app.press("label:Next edit");
  await app.waitFor("text:second");
  await app.press("text:Files");
  await app.waitFor("text:linked.rs");
  expect(app.enabled("role:treeitem[linked.rs]")).toBe(false);
  expect(app.enabled("role:treeitem[workspace.db]")).toBe(true);
  expect(app.shows("text:fast.ts")).toBe(false);
  await app.press("text:src");
  await app.press("text:slow.ts");
  await app.press("text:fast.ts");
  await app.waitFor("label:Preview of src/fast.ts");
  expect(app.shows("text:Binary file. No text preview available.")).toBe(false);
  await app.press("text:Changes");
  await app.waitFor("text:Edit 2 of 2");
  await app.waitFor("text:second");
  await app.press("label:All changed files");
  await app.waitFor("label:Open diff for src/fast.ts");
  await app.press("label:Open diff for src/fast.ts");
  await app.waitFor("text:first");
  await app.press("text:Files");
  await app.waitFor("label:Preview of src/fast.ts");
  await app.press("label:Close preview");
  expect(app.shows("label:Preview of src/fast.ts")).toBe(false);
  await app.press("text:src");
  await app.press("text:src");
  await app.press("text:empty.txt");
  await app.waitFor("label:Preview of src/empty.txt");
});

test("compact_files_panel_opens_from_the_header_and_returns_to_the_draft", async () => {
  const app = await open("../entries/session-files.tsx", { ...DESKTOP, width: 800, height: 560 });
  await app.type("label:Message", "Unsent");
  await app.press("label:Open workspace panel");
  await app.press("text:src");
  await app.press("text:binary.dat");
  await app.waitFor("text:Binary file. No text preview available.");
  await app.press("text:large.txt");
  await app.waitFor("text:This file exceeds the preview size limit.");
  await app.keyboard.press("Escape");
  await app.waitFor("label:Message");
  expect(app.value("label:Message")).toBe("Unsent");
});
