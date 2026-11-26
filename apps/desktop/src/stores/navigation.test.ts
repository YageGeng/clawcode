import { expect, test } from "nsh/test";

import { NavigationHistory } from "./navigation";
import type { AppLocation } from "./navigation";

/** A Location on host `h` with Session `s`. */
function at(sessionId: string | null, hostId = "h"): AppLocation {
  return { hostId, sessionId };
}

test("back_and_forward_walk_the_history", () => {
  const history = new NavigationHistory(at("a"));
  history.push(at("b"));
  history.push(at("c"));
  expect(history.back()).toEqual(at("b"));
  expect(history.back()).toEqual(at("a"));
  expect(history.canGoBack).toBe(false);
  expect(history.forward()).toEqual(at("b"));
  expect(history.current).toEqual(at("b"));
});

test("pushing_the_current_location_is_a_no_op", () => {
  const history = new NavigationHistory(at("a"));
  history.push(at("a"));
  history.push(at("a"));
  expect(history.back()).toBe(undefined);
});

test("push_after_back_drops_forward_entries", () => {
  const history = new NavigationHistory(at("a"));
  history.push(at("b"));
  history.push(at("c"));
  history.back();
  history.push(at("d"));
  expect(history.canGoForward).toBe(false);
  expect(history.back()).toEqual(at("b"));
});

test("history_keeps_the_newest_100", () => {
  const history = new NavigationHistory(at("s0"));
  for (let index = 1; index <= 150; index++) history.push(at(`s${index}`));
  let steps = 0;
  while (history.back() !== undefined) steps++;
  expect(steps).toBe(99);
  expect(history.current).toEqual(at("s51"));
});
