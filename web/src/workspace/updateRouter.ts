import type { SessionId, SessionUpdateNotification } from "../acp/protocol";
import type { EventOrder, EventSequenceRange, RecoveryCommit } from "../domain/model";
import { sessionWorkspace } from "./state";
import { reduceWorkspace } from "./state";
import type { WorkspaceAction, WorkspaceState } from "./state";
import { SessionWorkspaceStager } from "./sessionStaging";
import { SessionRecoveryBuffer } from "./recovery";
import { SessionUpdateDecoder } from "./updateDecoder";

export type WorkspaceStoreAccess = Readonly<{
  getState: () => WorkspaceState;
  dispatch: (action: WorkspaceAction) => void;
}>;

type RecoveryTracker = {
  eventCount: number;
  sequenceRange?: EventSequenceRange;
  order?: EventOrder;
};

/** Routes decoded ACP updates into one injected workspace projection. */
export class SessionUpdateRouter {
  private readonly decoder: SessionUpdateDecoder;
  private readonly store: WorkspaceStoreAccess;
  private readonly refreshSessionRuntime: (sessionId: SessionId) => Promise<void>;
  private readonly recovery: SessionRecoveryBuffer;
  private readonly recoveryTrackers = new Map<SessionId, RecoveryTracker>();
  private receivedOrder = 0;
  private recoveryId = 0;

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
    let workspaceDraft = current;
    const refreshRuntime = new Set<SessionId>();
    const touchedSessions = new Set<SessionId>();
    const stagers = new Map<SessionId, SessionWorkspaceStager>();
    try {
      for (const notification of notifications) {
        touchedSessions.add(notification.sessionId);
        const batch = this.recovery.accept(notification);
        if (batch === undefined) continue;
        for (const item of batch.notifications) {
          // One copy-on-write stager per Session keeps the collection copies and
          // transcript/event sorts at once per transport frame instead of once
          // per decoded action.
          let stager = stagers.get(item.sessionId);
          if (stager === undefined) {
            stager = new SessionWorkspaceStager(sessionWorkspace(current, item.sessionId));
            stagers.set(item.sessionId, stager);
          }
          const decoded = this.decoder.decode(item, stager.snapshot(), ++this.receivedOrder);
          if (decoded.scope === "global") {
            for (const action of decoded.actions) workspaceDraft = reduceWorkspace(workspaceDraft, action);
          } else {
            for (const action of decoded.actions) stager.apply(action);
          }
          if (decoded.refreshRuntime) refreshRuntime.add(batch.sessionId);
        }
        const tracker = this.recoveryTrackers.get(batch.sessionId);
        if (tracker !== undefined && batch.cursor !== undefined) {
          // Projection groups may coalesce several source events into one ACP
          // notification frame, so count the authoritative sequence range once.
          tracker.eventCount += batch.cursor.lastSequence - batch.cursor.sequence + 1;
          tracker.sequenceRange = {
            start: Math.min(tracker.sequenceRange?.start ?? batch.cursor.sequence, batch.cursor.sequence),
            end: Math.max(tracker.sequenceRange?.end ?? batch.cursor.lastSequence, batch.cursor.lastSequence)
          };
          tracker.order = {
            sequence: batch.cursor.lastSequence,
            receivedOrder: this.receivedOrder
          };
        }
        // Stage the cursor synchronously so the next projection group in this
        // physical frame validates against it. No observer can run before dispatch.
        this.recovery.commit(batch);
      }
      if (stagers.size > 0 || workspaceDraft !== current) {
        this.store.dispatch({ type: "workspace/committed", state: this.commitStaged(current, workspaceDraft, stagers) });
      }
    } catch (reason: unknown) {
      for (const sessionId of touchedSessions) {
        this.recovery.requireFullReplay(sessionId);
        // Drop any partially-populated summary so a later finishRecovery cannot
        // emit a divider that under-counts a frame whose replay never completed.
        this.recoveryTrackers.delete(sessionId);
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

  /** Merges staged Session projections into one committed WorkspaceState. */
  private commitStaged(
    current: WorkspaceState,
    workspaceDraft: WorkspaceState,
    stagers: ReadonlyMap<SessionId, SessionWorkspaceStager>
  ): WorkspaceState {
    if (stagers.size === 0) return workspaceDraft;
    const sessionWorkspaces = new Map(workspaceDraft.sessionWorkspaces);
    let runningSessionIds: Set<SessionId> | undefined;
    for (const [sessionId, stager] of stagers) {
      const committed = stager.commit();
      const base = sessionWorkspace(current, sessionId);
      if (committed.running !== base.running) {
        // Copy-on-write the running set only when a Session actually changed state.
        runningSessionIds ??= new Set(workspaceDraft.runningSessionIds);
        if (committed.running) runningSessionIds.add(sessionId);
        else runningSessionIds.delete(sessionId);
      }
      sessionWorkspaces.set(sessionId, committed);
    }
    return { ...workspaceDraft, sessionWorkspaces, ...(runningSessionIds === undefined ? {} : { runningSessionIds }) };
  }

  /** Returns every cursor whose complete projection group has reached the store. */
  recoveryCursors() {
    return this.recovery.cursors();
  }

  /** Starts a fresh replay summary for one Session resume request. */
  beginRecovery(sessionId: SessionId): void {
    this.recoveryTrackers.set(sessionId, { eventCount: 0 });
  }

  /** Returns the completed replay summary without retaining stale tracker state. */
  finishRecovery(sessionId: SessionId): RecoveryCommit | undefined {
    const tracker = this.recoveryTrackers.get(sessionId);
    this.recoveryTrackers.delete(sessionId);
    if (tracker?.sequenceRange === undefined || tracker.order === undefined) return undefined;
    return {
      notice: {
        recoveryId: `${sessionId}:${tracker.sequenceRange.start}:${tracker.sequenceRange.end}:${++this.recoveryId}`,
        eventCount: tracker.eventCount,
        sequenceRange: tracker.sequenceRange
      },
      order: tracker.order
    };
  }

  /** Drops an incomplete replay summary after a failed resume request. */
  cancelRecovery(sessionId: SessionId): void {
    this.recoveryTrackers.delete(sessionId);
  }

  /** Drops incomplete groups that cannot cross one WebSocket generation boundary. */
  discardPendingRecovery(): void {
    this.recovery.discardPending();
    this.recoveryTrackers.clear();
  }

  /** Forces one Session to recover from a full standard ACP replay. */
  requireFullReplay(sessionId: SessionId): void {
    this.recovery.requireFullReplay(sessionId);
  }

  /** Removes retained and partial recovery state for one deleted Session. */
  resetRecovery(sessionId: SessionId): void {
    this.recovery.reset(sessionId);
    this.recoveryTrackers.delete(sessionId);
  }
}
