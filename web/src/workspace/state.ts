import type { SessionId } from "../acp/protocol";
import type { PromptInput, SessionSummary } from "../domain/model";
import {
  initialSessionWorkspaceState,
  reduceSessionWorkspace
} from "./sessionState";
import type { SessionWorkspaceAction, SessionWorkspaceState } from "./sessionState";

export type ConnectionState =
  | { readonly type: "disconnected" }
  | { readonly type: "connecting"; readonly attempt: number }
  | { readonly type: "initializing" }
  | { readonly type: "ready" }
  | { readonly type: "reconnecting"; readonly attempt: number }
  | { readonly type: "failed"; readonly message: string };

export type WorkspaceState = Readonly<{
  connection: ConnectionState;
  sessions: readonly SessionSummary[];
  activeSessionId: SessionId | undefined;
  sessionWorkspaces: ReadonlyMap<SessionId, SessionWorkspaceState>;
  runningSessionIds: ReadonlySet<SessionId>;
  drafts: ReadonlyMap<SessionId, PromptInput>;
  composerRevision: number;
  diagnostics: readonly string[];
}>;

export type WorkspaceAction =
  | { readonly type: "connection/changed"; readonly connection: ConnectionState }
  | { readonly type: "session/listed"; readonly sessions: readonly SessionSummary[] }
  | { readonly type: "session/activated"; readonly sessionId: SessionId }
  | { readonly type: "session/deactivated" }
  | { readonly type: "session/title"; readonly sessionId: SessionId; readonly title: string }
  | { readonly type: "session/updated"; readonly sessionId: SessionId; readonly action: SessionWorkspaceAction }
  | { readonly type: "sessions/invalidated" }
  | { readonly type: "draft/activated"; readonly sessionId: SessionId; readonly draft: PromptInput }
  | { readonly type: "draft/changed"; readonly sessionId: SessionId; readonly draft: PromptInput }
  | { readonly type: "draft/cleared"; readonly sessionId: SessionId }
  | { readonly type: "diagnostic/added"; readonly message: string };

export const initialWorkspaceState: WorkspaceState = {
  connection: { type: "disconnected" },
  sessions: [],
  activeSessionId: undefined,
  sessionWorkspaces: new Map(),
  runningSessionIds: new Set(),
  drafts: new Map(),
  composerRevision: 0,
  diagnostics: []
};

/** Returns the current projection for one Session, or a shared empty projection before loading. */
export function sessionWorkspace(
  state: WorkspaceState,
  sessionId: SessionId
): SessionWorkspaceState {
  return state.sessionWorkspaces.get(sessionId) ?? initialSessionWorkspaceState;
}

/** Applies global actions and delegates Session actions to their isolated projection. */
export function reduceWorkspace(
  state: WorkspaceState,
  action: WorkspaceAction
): WorkspaceState {
  switch (action.type) {
    case "connection/changed": return { ...state, connection: action.connection };
    case "session/listed": {
      const sessionIds = new Set(action.sessions.map((session) => session.sessionId));
      const sessionWorkspaces = new Map(
        [...state.sessionWorkspaces].filter(([sessionId]) => sessionIds.has(sessionId))
      );
      const drafts = new Map(
        [...state.drafts].filter(([sessionId]) => sessionIds.has(sessionId))
      );
      const runningSessionIds = new Set(
        [...state.runningSessionIds].filter((sessionId) => sessionIds.has(sessionId))
      );
      const activeSessionId = state.activeSessionId !== undefined && sessionIds.has(state.activeSessionId)
        ? state.activeSessionId
        : undefined;
      return { ...state, sessions: action.sessions, activeSessionId, sessionWorkspaces, runningSessionIds, drafts };
    }
    case "session/activated": {
      if (state.sessionWorkspaces.has(action.sessionId)) {
        return { ...state, activeSessionId: action.sessionId };
      }
      const sessionWorkspaces = new Map(state.sessionWorkspaces);
      sessionWorkspaces.set(action.sessionId, initialSessionWorkspaceState);
      return { ...state, activeSessionId: action.sessionId, sessionWorkspaces };
    }
    case "session/deactivated": return { ...state, activeSessionId: undefined };
    case "session/title": return {
      ...state,
      sessions: state.sessions.map((session) => session.sessionId === action.sessionId ? { ...session, title: action.title } : session)
    };
    case "session/updated": {
      const sessionWorkspaces = new Map(state.sessionWorkspaces);
      const previous = sessionWorkspace(state, action.sessionId);
      const next = reduceSessionWorkspace(previous, action.action);
      sessionWorkspaces.set(
        action.sessionId,
        next
      );
      if (previous.running === next.running) return { ...state, sessionWorkspaces };
      const runningSessionIds = new Set(state.runningSessionIds);
      if (next.running) runningSessionIds.add(action.sessionId);
      else runningSessionIds.delete(action.sessionId);
      return { ...state, sessionWorkspaces, runningSessionIds };
    }
    case "sessions/invalidated": {
      const sessionWorkspaces = new Map(
        [...state.sessionWorkspaces].map(([sessionId, workspace]) => [
          sessionId,
          reduceSessionWorkspace(workspace, { type: "session/invalidated" })
        ])
      );
      return { ...state, sessionWorkspaces, runningSessionIds: new Set() };
    }
    case "draft/activated": {
      const drafts = new Map(state.drafts);
      drafts.set(action.sessionId, action.draft);
      return { ...state, drafts, composerRevision: state.composerRevision + 1 };
    }
    case "draft/changed": {
      const drafts = new Map(state.drafts);
      drafts.set(action.sessionId, action.draft);
      return { ...state, drafts };
    }
    case "draft/cleared": {
      const drafts = new Map(state.drafts);
      drafts.delete(action.sessionId);
      return { ...state, drafts };
    }
    case "diagnostic/added": return { ...state, diagnostics: [...state.diagnostics, action.message] };
  }
}
