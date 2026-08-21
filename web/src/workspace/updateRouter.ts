import type { SessionId, SessionUpdateNotification } from "../acp/protocol";
import { sessionWorkspace } from "./state";
import { reduceWorkspace } from "./state";
import type { WorkspaceAction, WorkspaceState } from "./state";
import { SessionRecoveryBuffer } from "./recovery";
import { SessionUpdateDecoder } from "./updateDecoder";

export type WorkspaceStoreAccess = Readonly<{
  getState: () => WorkspaceState;
  dispatch: (action: WorkspaceAction) => void;
}>;

/** Routes decoded ACP updates into one injected workspace projection. */
export class SessionUpdateRouter {
  private readonly decoder: SessionUpdateDecoder;
  private readonly store: WorkspaceStoreAccess;
  private readonly refreshSessionRuntime: (sessionId: SessionId) => Promise<void>;
  private readonly recovery: SessionRecoveryBuffer;
  private receivedOrder = 0;

  /** Creates a router whose state reads and writes use the same store instance. */
  constructor(
    namespace: string,
    store: WorkspaceStoreAccess,
    refreshSessionRuntime: (sessionId: SessionId) => Promise<void>
  ) {
    this.decoder = new SessionUpdateDecoder(namespace);
    this.recovery = new SessionRecoveryBuffer(namespace);
    this.store = store;
    this.refreshSessionRuntime = refreshSessionRuntime;
  }

  /** Routes every update into its owning Session, including background Sessions. */
  apply(notification: SessionUpdateNotification): void {
    this.applyMany([notification]);
  }

  /** Applies one transport frame while committing its UI actions only once. */
  applyMany(notifications: readonly SessionUpdateNotification[]): void {
    const current = this.store.getState();
    let projected = current;
    const refreshRuntime = new Set<SessionId>();
    const touchedSessions = new Set<SessionId>();
    try {
      for (const notification of notifications) {
        touchedSessions.add(notification.sessionId);
        const batch = this.recovery.accept(notification);
        if (batch === undefined) continue;
        for (const item of batch.notifications) {
          const decoded = this.decoder.decode(
            item,
            sessionWorkspace(projected, item.sessionId),
            ++this.receivedOrder
          );
          const decodedActions: WorkspaceAction[] = decoded.scope === "global"
            ? [...decoded.actions]
            : decoded.actions.map((action) => ({
                type: "session/updated" as const,
                sessionId: item.sessionId,
                action
              }));
          for (const action of decodedActions) projected = reduceWorkspace(projected, action);
          if (decoded.refreshRuntime) refreshRuntime.add(batch.sessionId);
        }
        // Stage the cursor synchronously so the next projection group in this
        // physical frame validates against it. No observer can run before dispatch.
        this.recovery.commit(batch);
      }
      if (projected !== current) {
        // The projection was already reduced in notification order above;
        // committing it directly avoids repeating every Map-heavy reducer.
        this.store.dispatch({ type: "workspace/committed", state: projected });
      }
    } catch (reason: unknown) {
      for (const sessionId of touchedSessions) {
        this.recovery.requireFullReplay(sessionId);
      }
      throw reason;
    }
    for (const sessionId of refreshRuntime) {
      void this.refreshSessionRuntime(sessionId).catch((reason: unknown) => {
        this.store.dispatch({
          type: "session/updated",
          sessionId,
          action: { type: "diagnostic/added", message: reason instanceof Error ? reason.message : String(reason) }
        });
      });
    }
  }

  /** Returns every cursor whose complete projection group has reached the store. */
  recoveryCursors() {
    return this.recovery.cursors();
  }

  /** Drops incomplete groups that cannot cross one WebSocket generation boundary. */
  discardPendingRecovery(): void {
    this.recovery.discardPending();
  }

  /** Forces one Session to recover from a full standard ACP replay. */
  requireFullReplay(sessionId: SessionId): void {
    this.recovery.requireFullReplay(sessionId);
  }

  /** Removes retained and partial recovery state for one deleted Session. */
  resetRecovery(sessionId: SessionId): void {
    this.recovery.reset(sessionId);
  }
}
