import type { SessionWorkspaceState } from "./sessionState";
import type { WorkspaceState } from "./state";

/** Resolves the active Session projection, or undefined when none is active. */
export function sessionWorkspaceOf(state: WorkspaceState): SessionWorkspaceState | undefined {
  return state.activeSessionId === undefined ? undefined : state.sessionWorkspaces.get(state.activeSessionId);
}
