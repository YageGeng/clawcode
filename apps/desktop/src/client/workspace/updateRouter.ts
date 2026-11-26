import type { SessionId, SessionUpdateNotification } from "../acp/protocol";
import { reduceWorkspace, sessionWorkspace } from "./state";
import type { WorkspaceAction, WorkspaceState } from "./state";
import { initialSessionWorkspaceState, SessionWorkspaceStager } from "./sessionState";
import type { SessionWorkspaceState } from "./sessionState";
import { SessionRecoveryBuffer } from "./recovery";
import { SessionUpdateDecoder } from "./updateDecoder";

export type WorkspaceStoreAccess = Readonly<{
  getState: () => WorkspaceState;
  dispatch: (action: WorkspaceAction) => void;
}>;

/** What the router asks its host to fetch again after a frame. */
export type RouterRefreshes = Readonly<{
  /** Re-reads one Session's run state (an idle update may race a new turn). */
  runtime: (sessionId: SessionId) => Promise<void>;
  /** Re-lists the Sessions (a spawn tool named a child the list does not have yet). */
  sessions: () => Promise<void>;
}>;

/** Routes decoded ACP updates into one injected workspace projection. */
export class SessionUpdateRouter {
  private readonly decoder: SessionUpdateDecoder;
  private readonly store: WorkspaceStoreAccess;
  private readonly refreshes: RouterRefreshes;
  private readonly recovery: SessionRecoveryBuffer;
  // A replacement history stays private until resume succeeds. Live state still goes to the workspace.
  private readonly replays = new Map<SessionId, { history: SessionWorkspaceStager; valid: boolean }>();
  private receivedOrder = 0;

  /** Creates a router whose state reads and writes use the same store instance. */
  constructor(namespace: string, store: WorkspaceStoreAccess, refreshes: RouterRefreshes) {
    this.decoder = new SessionUpdateDecoder(namespace);
    this.recovery = new SessionRecoveryBuffer(namespace);
    this.store = store;
    this.refreshes = refreshes;
  }

  /** Applies one transport frame's updates, committing their actions to the store once. */
  applyMany(notifications: readonly SessionUpdateNotification[]): void {
    const current = this.store.getState();
    let workspaceDraft = current;
    const refreshRuntime = new Set<SessionId>();
    let newChild = false;
    const touchedSessions = new Set<SessionId>();
    const stagers = new Map<SessionId, SessionWorkspaceStager>();
    try {
      for (const notification of notifications) {
        touchedSessions.add(notification.sessionId);
        const batch = this.recovery.accept(notification);
        if (batch === undefined) continue;
        for (const item of batch.notifications) {
          // One copy-on-write stager per Session copies each collection once per frame, not once per action.
          let stager = stagers.get(item.sessionId);
          if (stager === undefined) {
            stager = new SessionWorkspaceStager(sessionWorkspace(current, item.sessionId));
            stagers.set(item.sessionId, stager);
          }
          const history = this.replays.get(item.sessionId)?.history ?? stager;
          const entities = history.snapshot();
          const decoded = this.decoder.decode(item, entities, ++this.receivedOrder);
          if (decoded.scope === "global") {
            for (const action of decoded.actions) workspaceDraft = reduceWorkspace(workspaceDraft, action);
          } else {
            for (const action of decoded.actions) {
              if (action.type === "tool/upserted") {
                const child = action.tool.subagentSessionId;
                // List a child once, when its spawn tool first names it, not on every later update of that tool.
                if (child !== undefined && entities.tools.get(action.tool.toolCallId)?.subagentSessionId === undefined
                  && !workspaceDraft.sessions.some((session) => session.sessionId === child)) newChild = true;
              }
              if (action.type === "running/changed") stager.apply(action);
              else history.apply(action);
            }
          }
          if (decoded.refreshRuntime) refreshRuntime.add(batch.sessionId);
        }
        // Stage the cursor synchronously so the next projection group in this
        // physical frame validates against it. No observer can run before dispatch.
        this.recovery.commit(batch);
      }
      const committed = this.commitStaged(workspaceDraft, stagers);
      if (committed !== current) this.store.dispatch({ type: "workspace/committed", state: committed });
    } catch (reason: unknown) {
      for (const sessionId of touchedSessions) {
        this.recovery.requireFullReplay(sessionId);
        const replay = this.replays.get(sessionId);
        if (replay) replay.valid = false;
      }
      throw reason;
    }
    for (const sessionId of refreshRuntime) {
      void this.refreshes.runtime(sessionId).catch((reason: unknown) => console.warn(`refresh the run state of ${sessionId}: ${String(reason)}`));
    }
    // From the data path: the spawn card is not mounted while another Session is open,
    // so the sidebar would lag until a Run idles.
    if (newChild) void this.refreshes.sessions().catch(() => {});
  }

  /** Merges staged Session projections into one WorkspaceState; `workspaceDraft` itself when no Session changed. */
  private commitStaged(workspaceDraft: WorkspaceState, stagers: ReadonlyMap<SessionId, SessionWorkspaceStager>): WorkspaceState {
    let sessionWorkspaces: Map<SessionId, SessionWorkspaceState> | undefined;
    for (const [sessionId, stager] of stagers) {
      const committed = stager.commit();
      if (committed === sessionWorkspace(workspaceDraft, sessionId)) continue;
      sessionWorkspaces ??= new Map(workspaceDraft.sessionWorkspaces);
      sessionWorkspaces.set(sessionId, committed);
    }
    return sessionWorkspaces === undefined ? workspaceDraft : { ...workspaceDraft, sessionWorkspaces };
  }

  /** Streams a first history immediately, or replaces an existing history only after a complete replay. */
  async replay(sessionId: SessionId, resume: () => Promise<unknown>): Promise<void> {
    this.recovery.requireFullReplay(sessionId);
    if (!sessionWorkspace(this.store.getState(), sessionId).transcript.length) {
      this.store.dispatch({ type: "session/updated", sessionId, action: { type: "transcript/cleared" } });
      await resume();
      return;
    }

    const replay = { history: new SessionWorkspaceStager(initialSessionWorkspaceState), valid: true };
    this.replays.set(sessionId, replay);
    try {
      await resume();
      if (!replay.valid) throw new Error("Session history replay was incomplete");
      const state = this.store.getState();
      const current = state.sessionWorkspaces.get(sessionId);
      if (!current) return; // A directory refresh removed the Session while resume was pending.
      const replacement = { ...replay.history.commit(), running: current.running };
      const sessionWorkspaces = new Map(state.sessionWorkspaces).set(sessionId, replacement);
      this.store.dispatch({ type: "workspace/committed", state: { ...state, sessionWorkspaces } });
    } finally {
      this.replays.delete(sessionId);
    }
  }
}
