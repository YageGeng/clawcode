import { expect, test } from "nsh/test";

import type { SessionId, SessionUpdateNotification } from "./acp/protocol";
import { initialWorkspaceState, reduceWorkspace } from "./workspace/state";
import type { WorkspaceAction, WorkspaceState } from "./workspace/state";
import { SessionUpdateRouter } from "./workspace/updateRouter";
import type { WorkspaceStoreAccess } from "./workspace/updateRouter";

test("router_dispatches_through_the_injected_store", () => {
  let state: WorkspaceState = initialWorkspaceState;
  const actions: WorkspaceAction[] = [];
  const store: WorkspaceStoreAccess = {
    getState: () => state,
    dispatch: (action) => {
      actions.push(action);
      state = reduceWorkspace(state, action);
    },
  };
  const idle = () => Promise.resolve();
  const router = new SessionUpdateRouter("clawcode", store, idle, idle, idle);
  const sessionId = "session-1" as SessionId;
  const notification = {
    sessionId,
    update: {
      sessionUpdate: "agent_message_chunk",
      messageId: "message-1",
      content: { type: "text", text: "你好 🌍\nline two" },
      _meta: { clawcode: { turnId: "turn-1", timestampMs: "1700000000000", sequence: 1 } },
    },
  } as unknown as SessionUpdateNotification;

  router.apply(notification);

  expect(actions.length).toBeGreaterThan(0);
  const session = state.sessionWorkspaces.get(sessionId);
  expect(session?.transcript.length).toBe(1);
  expect(JSON.stringify([...(session?.messages.values() ?? [])])).toContain("你好 🌍\\nline two");
});
