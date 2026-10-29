import type { SessionWorkspaceState } from "./sessionState";
import type { WorkspaceState } from "./state";
import type { SessionId } from "../acp/protocol";

/** Resolves the active Session projection, or undefined when none is active. */
export function sessionWorkspaceOf(state: WorkspaceState, sessionId: SessionId | undefined = state.activeSessionId): SessionWorkspaceState | undefined {
  return sessionId === undefined ? undefined : state.sessionWorkspaces.get(sessionId);
}
