import type { SessionId, SessionUpdateNotification } from "../acp/protocol";
import { reduceWorkspace, sessionWorkspace } from "./state";
import type { WorkspaceAction, WorkspaceState } from "./state";
import { SessionWorkspaceStager } from "./sessionState";
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
          const entities = stager.snapshot();
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
              stager.apply(action);
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
      for (const sessionId of touchedSessions) this.recovery.requireFullReplay(sessionId);
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

  /** Forces one Session to recover from a full standard ACP replay. */
  requireFullReplay(sessionId: SessionId): void {
    this.recovery.requireFullReplay(sessionId);
  }
}
