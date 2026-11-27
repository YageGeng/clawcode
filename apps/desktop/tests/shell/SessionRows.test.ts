import { expect, test } from "nsh/test";

import type { SessionId } from "../../src/client/acp/protocol";
import type { SessionSummary } from "../../src/client/domain/model";
import { sidebarRows } from "../../src/shell/SessionRows";

/** A listed Session in `/work`, optionally owned by a parent as a subagent. */
function listed(sessionId: string, owner?: string): SessionSummary {
  return {
    sessionId: sessionId as SessionId, cwd: "/work", title: sessionId,
    ...(owner ? { owner: { sessionId: owner as SessionId, name: "worker", nickname: "Ada" } } : {}),
  };
}

test("subagent sessions stay out of the sidebar", () => {
  const rows = sidebarRows([listed("root"), listed("child", "root")], new Set(["/work"]));
  const ids = rows.flatMap((row) => row.kind === "session" ? [row.session.sessionId] : []);
  expect(ids).toEqual(["root", "root"]);
});

test("pinned_sessions_lead_the_sidebar_and_leave_recent", () => {
  const rows = sidebarRows([listed("a"), listed("b")], new Set(), new Set(["b" as SessionId]));
  const outline = rows.map((row) => row.kind === "session" ? `${row.key.split(":")[0]}:${row.session.sessionId}` : row.kind === "heading" ? `#${row.label}` : row.kind);
  expect(outline).toEqual(["#Pinned", "pinned:b", "#Workspaces", "workspace", "#Recent", "recent:a"]);
});
