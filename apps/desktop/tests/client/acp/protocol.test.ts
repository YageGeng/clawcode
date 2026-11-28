import { expect, test } from "nsh/test";

import { AcpProtocol } from "../../../src/client/acp/protocol";
import type { MessageId, SessionInfo } from "../../../src/client/acp/protocol";

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

test("tree_entry_finds_a_message_and_its_parent", () => {
  const tree = { entries: [
    { entryId: "e1", parentId: null, kind: "message", payload: { message_id: "u1", content: { type: "user" } } },
    { entryId: "e2", parentId: "e1", kind: "message", payload: { message_id: "a1", content: { type: "assistant" } } },
    { entryId: "e3", parentId: "e2", kind: "compaction", payload: { message_id: "u2" } },
    { entryId: "e4", parentId: "e3", kind: "message", payload: { message_id: "u2", content: { type: "user" } } },
  ] };
  expect(AcpProtocol.treeEntry(tree, "u1" as MessageId, "user")).toEqual({ entryId: "e1", parentId: null });
  expect(AcpProtocol.treeEntry(tree, "u2" as MessageId, "user")).toEqual({ entryId: "e4", parentId: "e3" });
  expect(AcpProtocol.treeEntry(tree, "a1" as MessageId, "assistant")).toEqual({ entryId: "e2", parentId: "e1" });
  // The role must match, and a missing id finds nothing.
  expect(AcpProtocol.treeEntry(tree, "a1" as MessageId, "user")).toBeUndefined();
  expect(AcpProtocol.treeEntry(tree, "nope" as MessageId, "user")).toBeUndefined();
  expect(() => AcpProtocol.treeEntry({}, "u1" as MessageId, "user")).toThrow();
});
