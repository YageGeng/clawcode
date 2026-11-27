import { Storage } from "nsh";
import { expect, test } from "nsh/test";

import type { SessionId } from "../../src/client/acp/protocol";
import { createPinStore } from "../../src/stores/pins";

test("pins_are_per_host_and_survive_a_reload", async () => {
  await Storage.clear();
  const pins = createPinStore();
  await pins.load();
  pins.toggle("local", "s1" as SessionId);
  pins.toggle("peer", "s9" as SessionId);
  pins.toggle("local", "s2" as SessionId);
  pins.toggle("local", "s1" as SessionId);
  expect([...pins.pinned("local")]).toEqual(["s2"]);
  await new Promise((resolve) => setTimeout(resolve, 50));
  const reopened = createPinStore();
  await reopened.load();
  expect([...reopened.pinned("local")]).toEqual(["s2"]);
  expect([...reopened.pinned("peer")]).toEqual(["s9"]);
});

test("unreadable_pins_start_empty", async () => {
  await Storage.setItem("pins", "{");
  const pins = createPinStore();
  await pins.load();
  expect(pins.pinned("local").size).toBe(0);
});
