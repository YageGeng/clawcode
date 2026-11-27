import { expect, test } from "nsh/test";

import { AcpProtocol } from "../../../src/client/acp/protocol";
import type { SessionInfo } from "../../../src/client/acp/protocol";

/** A listed Session whose product metadata carries `details`. */
function listed(details: Record<string, unknown>): SessionInfo {
  return { sessionId: "s-child", cwd: "/work", title: "Child", _meta: { clawcode: details } } as unknown as SessionInfo;
}

test("sessionSummary keeps parentSessionId from product meta", () => {
  expect(AcpProtocol.sessionSummary(listed({ parentSessionId: "s-parent" }), "clawcode").parentSessionId).toBe("s-parent");
  expect("parentSessionId" in AcpProtocol.sessionSummary(listed({ parentSessionId: null }), "clawcode")).toBe(false);
  expect("parentSessionId" in AcpProtocol.sessionSummary(listed({}), "clawcode")).toBe(false);
});

test("sessionSummary keeps a complete ownership edge", () => {
  const owned = AcpProtocol.sessionSummary(listed({ ownership: { sessionId: "s-root", name: "worker", nickname: "Ada" } }), "clawcode");
  expect(owned.owner).toEqual({ sessionId: "s-root", name: "worker", nickname: "Ada" });
  expect("owner" in AcpProtocol.sessionSummary(listed({ ownership: null }), "clawcode")).toBe(false);
  expect("owner" in AcpProtocol.sessionSummary(listed({ ownership: { sessionId: "s-root", name: "worker" } }), "clawcode")).toBe(false);
});
