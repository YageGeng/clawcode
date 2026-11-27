import { expect, test } from "nsh/test";

import type { SessionId, TimestampMs } from "../../src/client/acp/protocol";
import type { SessionSummary } from "../../src/client/domain/model";
import { sessionRelations, subagentActivity } from "../../src/client/relations";

/** A listed Session with `extra` fields. */
function listed(sessionId: string, extra: Partial<SessionSummary> = {}): SessionSummary {
  return { sessionId: sessionId as SessionId, cwd: "/work", title: sessionId, ...extra };
}

/** A subagent of `owner` last modified at `modifiedAtMs`. */
function child(sessionId: string, owner: string, modifiedAtMs: string): SessionSummary {
  return listed(sessionId, { modifiedAtMs: modifiedAtMs as TimestampMs, owner: { sessionId: owner as SessionId, name: sessionId, nickname: sessionId.toUpperCase() } });
}

test("relations split running and previous subagents", () => {
  const sessions = [listed("origin"), listed("s", { parentSessionId: "origin" as SessionId }),
    child("a", "s", "1"), child("b", "s", "3"), child("c", "s", "2"), child("other", "origin", "4")];
  const relations = sessionRelations(sessions, "s" as SessionId, (id) => id === "a" || id === "b");
  expect(relations.parent?.sessionId).toBe("origin");
  expect(relations.running.map((item) => item.sessionId)).toEqual(["b", "a"]);
  expect(relations.previous.map((item) => item.sessionId)).toEqual(["c"]);
});

test("relations are empty for a session without parent or children", () => {
  const relations = sessionRelations([listed("s")], "s" as SessionId, () => true);
  expect(relations.parent).toBe(undefined);
  expect(relations.running.length).toBe(0);
  expect(relations.previous.length).toBe(0);
});

test("relations use ownership, not fork parents, for subagents", () => {
  const sessions = [listed("s"), listed("fork", { parentSessionId: "s" as SessionId })];
  const relations = sessionRelations(sessions, "s" as SessionId, () => false);
  expect(relations.running.length + relations.previous.length).toBe(0);
});

test("subagents waiting on the user or reported by the host count as active", () => {
  const host = {
    running: (id: SessionId) => id === "open",
    permissionFor: (id: SessionId) => id === "approval" ? ({} as never) : undefined,
    awaitsAnswer: (id: SessionId) => id === "question",
  };
  const active = subagentActivity(host, new Set(["queried" as SessionId]));
  expect(["open", "approval", "question", "queried", "idle"].map((id) => active(id as SessionId))).toEqual([true, true, true, true, false]);
});

test("a_subagent_relates_back_to_the_agent_that_spawned_it", () => {
  const sessions = [listed("root"), child("a", "root", "1")];
  expect(sessionRelations(sessions, "a" as SessionId, () => false).owner?.sessionId).toBe("root");
  expect(sessionRelations(sessions, "root" as SessionId, () => false).owner).toBe(undefined);
});
