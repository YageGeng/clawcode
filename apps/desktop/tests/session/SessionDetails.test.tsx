import { expect, open, test } from "nsh/test";

import { DESKTOP } from "../support/desktop";

/** Wide enough for the card to float beside the 760 pt transcript without covering it. */
const WIDE = { ...DESKTOP, width: 1720 };

test("a_wide_session_shows_the_details_card_with_host_and_relations", async () => {
  const app = await open("../entries/session-details.tsx", WIDE);
  await app.waitFor("text:Related sessions · 1 running");
  expect(app.shows("text:This computer")).toBe(true);
  expect(app.shows("label:Open in file manager")).toBe(true);
  expect(app.shows("text:Original plan")).toBe(true);
  expect(app.shows("label:Ada, running")).toBe(true);
  expect(app.shows("text:Bo")).toBe(true);
  expect(app.shows("label:Session details")).toBe(false);
  await app.press("text:Ada");
  await app.waitFor("text:Lexer");
  expect(app.shows("text:Forked from")).toBe(false);
});

test("a_medium_session_moves_the_transcript_aside_to_show_the_card", async () => {
  const app = await open("../entries/session-details.tsx", { ...DESKTOP, width: 1400 });
  await app.waitFor("text:Forked from");
  expect(app.shows("label:Session details")).toBe(false);
  // The reply's column ends left of the card instead of running under it.
  const reply = app.layout("text:Started two helpers.")!;
  const card = app.layout("text:Forked from")!;
  expect(reply.x + reply.width <= card.x).toBe(true);
});

test("a_narrow_session_opens_the_details_card_from_the_header", async () => {
  const app = await open("../entries/session-details.tsx", DESKTOP);
  await app.waitFor("label:Session details");
  expect(app.shows("text:Forked from")).toBe(false);
  await app.press("label:Session details");
  await app.waitFor("text:Forked from");
  expect(app.shows("label:Open in file manager")).toBe(true);
});

test("a_remote_host_has_no_open_in_and_hides_spinners_offline", async () => {
  const app = await open("../entries/session-details-remote.tsx", WIDE);
  await app.waitFor("label:Ada, running");
  expect(app.shows("text:Workstation")).toBe(true);
  expect(app.shows("label:Open in file manager")).toBe(false);
  // Browsing files drops this fixture's connection; closing the panel brings the floating card back.
  await app.press("label:Open workspace panel");
  await app.waitFor("text:Offline");
  await app.press("label:Close workspace panel");
  // Offline, nothing counts as running: the subagent moves under "earlier".
  await app.waitFor("text:Related sessions");
  expect(app.shows("label:Ada, running")).toBe(false);
});

test("a_subagent_card_returns_to_its_parent_agent", async () => {
  const app = await open("../entries/session-details.tsx", WIDE);
  await app.waitFor("text:Ada");
  await app.press("text:Ada");
  await app.waitFor("text:Parent agent");
  await app.press("text:Parent agent");
  await app.waitFor("text:Forked from");
});

test("editor_choices_unfold_inside_the_card", async () => {
  const app = await open("../entries/session-details.tsx", WIDE);
  await app.waitFor("label:Choose an editor");
  const before = app.texts().length;
  await app.press("label:Choose an editor");
  // The file manager is always a choice; the list grows in place instead of opening a menu.
  expect(app.texts().length).toBeGreaterThan(before);
  expect(app.menu()).toBe(null);
  await app.press("label:Choose an editor");
  expect(app.texts().length).toBe(before);
});
