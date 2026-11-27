import { expect, test } from "nsh/test";

import { AGENT_HUES, agentHue } from "../../src/ui/agentAvatar";

test("an agent's hue depends only on its name", () => {
  expect(agentHue("Ohm")).toBe(agentHue("Ohm"));
  expect(AGENT_HUES.includes(agentHue("Archimedes"))).toBe(true);
  // The nickname pool spreads over several colors rather than collapsing onto one.
  const hues = new Set(["Ohm", "Pascal", "Archimedes", "Boole", "Hume", "Newton", "Carver", "Euclid"].map(agentHue));
  expect(hues.size >= 4).toBe(true);
});
