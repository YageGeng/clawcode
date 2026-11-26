import type { SessionId } from "../acp/protocol";
import type { PermissionPrompt, SessionNotice, SessionSummary } from "../domain/model";
import { initialSessionWorkspaceState, reduceSessionWorkspace } from "./sessionState";
import type { SessionWorkspaceAction, SessionWorkspaceState } from "./sessionState";

/** Everything one host's Sessions show. */
export type WorkspaceState = Readonly<{
  sessions: readonly SessionSummary[];
  /** Projections of the Sessions opened on this connection. */
  sessionWorkspaces: ReadonlyMap<SessionId, SessionWorkspaceState>;
  /** Notices not yet shown as toasts. */
  notices: readonly SessionNotice[];
  permissions: readonly PermissionPrompt[];
}>;

export type WorkspaceAction =
  | { readonly type: "workspace/committed"; readonly state: WorkspaceState }
  | { readonly type: "session/listed"; readonly sessions: readonly SessionSummary[] }
  | { readonly type: "session/title"; readonly sessionId: SessionId; readonly title: string }
  | { readonly type: "session/updated"; readonly sessionId: SessionId; readonly action: SessionWorkspaceAction }
  /** The connection ended: nothing finishes a turn on it, so no Session runs any more. */
  | { readonly type: "sessions/stopped" }
  | { readonly type: "notice/added"; readonly notice: SessionNotice }
  | { readonly type: "notice/dismissed"; readonly id: string }
  | { readonly type: "permission/asked"; readonly prompt: PermissionPrompt }
  | { readonly type: "permission/settled"; readonly key: string };

export const initialWorkspaceState: WorkspaceState = {
  sessions: [],
  sessionWorkspaces: new Map(),
  notices: [],
  permissions: []
};

/** Returns the current projection for one Session, or a shared empty projection before loading. */
export function sessionWorkspace(state: WorkspaceState, sessionId: SessionId): SessionWorkspaceState {
  return state.sessionWorkspaces.get(sessionId) ?? initialSessionWorkspaceState;
}

/** Applies global actions and delegates Session actions to their isolated projection. */
export function reduceWorkspace(state: WorkspaceState, action: WorkspaceAction): WorkspaceState {
  switch (action.type) {
    // SessionUpdateRouter has already reduced an entire transport frame against
    // one local projection, so install that result without replaying its actions.
    case "workspace/committed": return action.state;
    case "session/listed": {
      const sessionIds = new Set(action.sessions.map((session) => session.sessionId));
      const sessionWorkspaces = new Map([...state.sessionWorkspaces].filter(([sessionId]) => sessionIds.has(sessionId)));
      // The backend lists Sessions in directory order; creation order keeps
      // children in spawn order and appends new ones instead of reshuffling.
      const sessions = [...action.sessions].sort((left, right) => {
        const leftTime = BigInt(left.createdAtMs ?? 0);
        const rightTime = BigInt(right.createdAtMs ?? 0);
        return leftTime === rightTime ? 0 : leftTime < rightTime ? -1 : 1;
      });
      return { ...state, sessions, sessionWorkspaces };
    }
    case "session/title": return {
      ...state,
      sessions: state.sessions.map((session) => session.sessionId === action.sessionId ? { ...session, title: action.title } : session)
    };
    case "session/updated": {
      const previous = sessionWorkspace(state, action.sessionId);
      const next = reduceSessionWorkspace(previous, action.action);
      if (next === previous) return state;
      return { ...state, sessionWorkspaces: new Map(state.sessionWorkspaces).set(action.sessionId, next) };
    }
    case "sessions/stopped": {
      const running = [...state.sessionWorkspaces].filter(([, session]) => session.running);
      if (running.length === 0) return state;
      const sessionWorkspaces = new Map(state.sessionWorkspaces);
      for (const [sessionId, session] of running) sessionWorkspaces.set(sessionId, { ...session, running: false });
      return { ...state, sessionWorkspaces };
    }
    case "notice/added": return { ...state, notices: [...state.notices, action.notice] };
    case "notice/dismissed": return { ...state, notices: state.notices.filter((notice) => notice.id !== action.id) };
    case "permission/asked": return { ...state, permissions: [...state.permissions, action.prompt] };
    case "permission/settled": return { ...state, permissions: state.permissions.filter((prompt) => prompt.key !== action.key) };
  }
}
