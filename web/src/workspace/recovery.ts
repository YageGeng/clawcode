import { AcpProtocol } from "../acp/protocol";
import type {
  EventSequence,
  ProjectionGroupMeta,
  RunId,
  SessionId,
  SessionRecoveryCursor,
  SessionUpdateNotification
} from "../acp/protocol";

export type RecoveryBatch = Readonly<{
  sessionId: SessionId;
  notifications: readonly SessionUpdateNotification[];
  cursor?: Readonly<{
    runId: RunId;
    sequence: EventSequence;
    lastSequence: EventSequence;
    operationPhase?: ProjectionGroupMeta["operationPhase"];
  }>;
}>;

type PendingGroup = {
  runId: RunId;
  sequence: EventSequence;
  lastSequence: EventSequence;
  count: number;
  operationPhase?: ProjectionGroupMeta["operationPhase"];
  notifications: Map<number, SessionUpdateNotification>;
};

/** Buffers recoverable notifications until one Kernel projection group is complete. */
export class SessionRecoveryBuffer {
  private readonly namespace: string;
  private readonly committed = new Map<SessionId, SessionRecoveryCursor>();
  private readonly pending = new Map<SessionId, PendingGroup>();
  private readonly fullOnly = new Set<SessionId>();

  /** Creates a buffer for one ACP product metadata namespace. */
  constructor(namespace: string) {
    this.namespace = namespace;
  }

  /** Accepts one notification and returns only complete, atomically applicable groups. */
  accept(notification: SessionUpdateNotification): RecoveryBatch | undefined {
    const group = AcpProtocol.projectionGroupMeta(notification._meta, this.namespace);
    if (group === undefined) {
      return { sessionId: notification.sessionId, notifications: [notification] };
    }
    const event = AcpProtocol.eventMeta(notification._meta, this.namespace);
    if (event === undefined || !AcpProtocol.validSequence(event.sequence)) {
      this.requireFullReplay(notification.sessionId);
      throw new Error("Recoverable ACP notification is missing a valid event sequence");
    }
    const sequence = event.sequence as EventSequence;
    const lastSequence = group.lastSequence ?? sequence;
    if (!AcpProtocol.validSequence(lastSequence) || lastSequence < sequence) {
      this.requireFullReplay(notification.sessionId);
      throw new Error("ACP projection sequence range is invalid");
    }
    const cursor = this.committed.get(notification.sessionId);
    if (this.fullOnly.has(notification.sessionId) && group.operationPhase !== "start") {
      return undefined;
    }
    if (cursor !== undefined && (cursor.runId !== group.runId || sequence < cursor.nextSequence)) {
      this.requireFullReplay(notification.sessionId);
      throw new Error("ACP projection sequence moved behind the committed cursor");
    }
    const current = this.pending.get(notification.sessionId);
    let pending = current;
    if (pending === undefined) {
      if (cursor === undefined && group.operationPhase !== "start") {
        this.requireFullReplay(notification.sessionId);
        return undefined;
      }
      pending = {
        runId: group.runId,
        sequence,
        lastSequence,
        count: group.projectionCount,
        ...(group.operationPhase === undefined ? {} : { operationPhase: group.operationPhase }),
        notifications: new Map()
      };
      this.pending.set(notification.sessionId, pending);
    } else if (
      pending.runId !== group.runId
      || pending.sequence !== sequence
      || pending.lastSequence !== lastSequence
      || pending.count !== group.projectionCount
      || pending.operationPhase !== group.operationPhase
    ) {
      this.requireFullReplay(notification.sessionId);
      throw new Error("ACP projection group changed before it was complete");
    }
    if (pending.notifications.has(group.projectionIndex)) {
      this.requireFullReplay(notification.sessionId);
      throw new Error("ACP projection group contains a duplicate index");
    }
    pending.notifications.set(group.projectionIndex, notification);
    if (pending.notifications.size !== pending.count) return undefined;
    const notifications = [...pending.notifications.entries()]
      .sort(([left], [right]) => left - right)
      .map(([, value]) => value);
    this.pending.delete(notification.sessionId);
    return {
      sessionId: notification.sessionId,
      notifications,
      cursor: {
        runId: pending.runId,
        sequence: pending.sequence,
        lastSequence: pending.lastSequence,
        ...(pending.operationPhase === undefined ? {} : { operationPhase: pending.operationPhase })
      }
    };
  }

  /** Advances the durable client cursor only after its UI batch was committed. */
  commit(batch: RecoveryBatch): void {
    if (batch.cursor === undefined) return;
    if (batch.cursor.operationPhase === "end") {
      this.committed.delete(batch.sessionId);
      this.fullOnly.delete(batch.sessionId);
      return;
    }
    this.committed.set(batch.sessionId, {
      sessionId: batch.sessionId,
      runId: batch.cursor.runId,
      // A coalesced notification atomically covers every source sequence
      // through lastSequence, while ordinary groups default it to sequence.
      nextSequence: (batch.cursor.lastSequence + 1) as EventSequence
    });
    this.fullOnly.delete(batch.sessionId);
  }

  /** Returns every complete cursor safe to advertise during Initialize. */
  cursors(): readonly SessionRecoveryCursor[] {
    return [...this.committed.values()];
  }

  /** Discards every incomplete transport group while preserving committed cursors. */
  discardPending(): void {
    this.pending.clear();
  }

  /** Discards partial and committed state until a new full replay start arrives. */
  requireFullReplay(sessionId: SessionId): void {
    this.pending.delete(sessionId);
    this.committed.delete(sessionId);
    this.fullOnly.add(sessionId);
  }

  /** Clears all recovery state for one Session. */
  reset(sessionId: SessionId): void {
    this.pending.delete(sessionId);
    this.committed.delete(sessionId);
    this.fullOnly.delete(sessionId);
  }
}
