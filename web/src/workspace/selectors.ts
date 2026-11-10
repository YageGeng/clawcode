import type { SessionWorkspaceState } from "./sessionState";
import type { WorkspaceState } from "./state";
import type { SessionId } from "../acp/protocol";

/** Resolves the active Session projection, or undefined when none is active. */
export function sessionWorkspaceOf(state: WorkspaceState, sessionId: SessionId | undefined = state.activeSessionId): SessionWorkspaceState | undefined {
  return sessionId === undefined ? undefined : state.sessionWorkspaces.get(sessionId);
}

/**
 * Counts exec_command cards whose terminal is open and has not exited yet; a
 * card cancelled by Esc still counts while its process runs. A failed card
 * never started a process (blocked, invalid, or refused), so its terminal
 * stays open forever and must not count.
 *
 * shortcut: a call cancelled in the moment before its process spawned looks like
 * a cancelled running one and keeps counting, replay included; return exec
 * details from a cancelled wait so cancelled cards can be excluded if that shows up.
 */
export function backgroundTerminalCount(session: SessionWorkspaceState): number {
  let count = 0;
  for (const tool of session.tools.values()) {
    if (tool.title !== "exec_command" || tool.status === "failed") continue;
    const terminal = session.terminals.get(tool.toolCallId);
    if (terminal !== undefined && terminal.exitStatus === undefined) count += 1;
  }
  return count;
}
