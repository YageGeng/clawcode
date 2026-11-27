import type { AvailableCommand } from "../acp/commands";
import type { SessionId } from "../acp/protocol";
import type { SessionConfigSelect } from "../acp/sessionConfig";
import type { PermissionPrompt, SessionNotice, SessionSummary } from "../domain/model";
import { initialSessionWorkspaceState, reduceSessionWorkspace } from "./sessionState";
import type { SessionWorkspaceAction, SessionWorkspaceState } from "./sessionState";
import { initialInteractions, questionId } from "./interactions";

/** Everything one host's Sessions show. */
export type WorkspaceState = Readonly<{
  sessions: readonly SessionSummary[];
  /** Projections of the Sessions opened on this connection. */
  sessionWorkspaces: ReadonlyMap<SessionId, SessionWorkspaceState>;
  /** Sidebar attention changes independently of streamed transcript content. */
  awaitingAnswers: ReadonlySet<SessionId>;
  /** Complete command lists last advertised for individual Sessions on this host. */
  sessionCommands: ReadonlyMap<SessionId, readonly AvailableCommand[]>;
  /** Confirmed host settings have their own lifetime, independent of transcript replay. */
  sessionConfigs: ReadonlyMap<SessionId, readonly SessionConfigSelect[]>;
  /** Notices not yet shown as toasts. */
  notices: readonly SessionNotice[];
  permissions: readonly PermissionPrompt[];
}>;

export type WorkspaceAction =
  | { readonly type: "workspace/committed"; readonly state: WorkspaceState }
  | { readonly type: "session/listed"; readonly sessions: readonly SessionSummary[] }
  | { readonly type: "session/title"; readonly sessionId: SessionId; readonly title: string }
  | { readonly type: "commands/replaced"; readonly sessionId: SessionId; readonly commands: readonly AvailableCommand[] }
  | { readonly type: "config/replaced"; readonly sessionId: SessionId; readonly options: readonly SessionConfigSelect[] }
  | { readonly type: "session/updated"; readonly sessionId: SessionId; readonly action: SessionWorkspaceAction }
  /** A new connection rebuilds questions from its own replay and live asks. */
  | { readonly type: "questions/invalidated" }
  /** The connection ended: nothing finishes a turn on it, so no Session runs any more. */
  | { readonly type: "sessions/stopped" }
  | { readonly type: "notice/added"; readonly notice: SessionNotice }
  | { readonly type: "notice/dismissed"; readonly id: string }
  | { readonly type: "permission/asked"; readonly prompt: PermissionPrompt }
  | { readonly type: "permission/settled"; readonly key: string };

export const initialWorkspaceState: WorkspaceState = {
  sessions: [],
  sessionWorkspaces: new Map(),
  awaitingAnswers: new Set(),
  sessionConfigs: new Map(),
  sessionCommands: new Map(),
  notices: [],
  permissions: []
};

/** Returns the current projection for one Session, or a shared empty projection before loading. */
export function sessionWorkspace(state: WorkspaceState, sessionId: SessionId): SessionWorkspaceState {
  return state.sessionWorkspaces.get(sessionId) ?? initialSessionWorkspaceState;
}

/** Publishes changed projections and updates the sidebar index only for Sessions whose questions changed. */
export function withSessionWorkspaces(state: WorkspaceState, replacements: Iterable<readonly [SessionId, SessionWorkspaceState]>): WorkspaceState {
  let sessions: Map<SessionId, SessionWorkspaceState> | undefined;
  let awaiting: Set<SessionId> | undefined;
  for (const [id, next] of replacements) {
    const previous = state.sessionWorkspaces.get(id);
    if (previous === next) continue;
    (sessions ??= new Map(state.sessionWorkspaces)).set(id, next);
    const { questions, queued } = next.interactions;
    if (previous?.interactions.questions === questions && previous.interactions.queued === queued) continue;
    const open = questions.some(({ key }) => !queued.has(questionId(key)));
    if (open === state.awaitingAnswers.has(id)) continue;
    awaiting ??= new Set(state.awaitingAnswers);
    if (open) awaiting.add(id);
    else awaiting.delete(id);
  }
  return sessions ? { ...state, sessionWorkspaces: sessions, awaitingAnswers: awaiting ?? state.awaitingAnswers } : state;
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
      const sessionConfigs = new Map([...state.sessionConfigs].filter(([sessionId]) => sessionIds.has(sessionId)));
      const sessionCommands = new Map([...state.sessionCommands].filter(([sessionId]) => sessionIds.has(sessionId)));
      const awaitingAnswers = new Set([...state.awaitingAnswers].filter((id) => sessionIds.has(id)));
      return { ...state, sessions, sessionWorkspaces, sessionConfigs, sessionCommands, awaitingAnswers };
    }
    case "commands/replaced": return { ...state, sessionCommands: new Map(state.sessionCommands).set(action.sessionId, action.commands) };
    case "config/replaced": return { ...state, sessionConfigs: new Map(state.sessionConfigs).set(action.sessionId, action.options) };
    case "session/title": return {
      ...state,
      sessions: state.sessions.map((session) => session.sessionId === action.sessionId ? { ...session, title: action.title } : session)
    };
    case "session/updated": {
      const previous = sessionWorkspace(state, action.sessionId);
      const next = reduceSessionWorkspace(previous, action.action);
      if (next === previous) return state;
      return withSessionWorkspaces(state, [[action.sessionId, next]]);
    }
    case "questions/invalidated": {
      const replacements: [SessionId, SessionWorkspaceState][] = [];
      for (const [id, session] of state.sessionWorkspaces) {
        const { questions, queued, settled, plan } = session.interactions;
        if (questions.length || queued.size || settled.size) {
          // Keep the readable transcript and plan. A new live ask must not revive an unrelated cached question.
          replacements.push([id, { ...session, interactions: { ...initialInteractions, plan } }]);
        }
      }
      return withSessionWorkspaces(state, replacements);
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
