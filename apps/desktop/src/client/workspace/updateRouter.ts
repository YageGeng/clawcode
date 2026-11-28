import type { SessionId, SessionUpdateNotification } from "../acp/protocol";
import { reduceWorkspace, sessionWorkspace, withSessionWorkspaces } from "./state";
import type { WorkspaceAction, WorkspaceState } from "./state";
import { initialSessionWorkspaceState, SessionWorkspaceStager } from "./sessionState";
import type { SessionWorkspaceState } from "./sessionState";
import type { InteractionChange } from "./interactions";
import { SessionRecoveryBuffer } from "./recovery";
import { SessionUpdateDecoder } from "./updateDecoder";

export type WorkspaceStoreAccess = Readonly<{
  getState: () => WorkspaceState;
  dispatch: (action: WorkspaceAction) => void;
}>;

/** What the router asks its host to fetch again after a frame. */
export type RouterRefreshes = Readonly<{
  /** Replays the new active branch after another client navigates the Session tree. */
  branch: (sessionId: SessionId) => Promise<void>;
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
  // A replacement history stays private until resume succeeds. The wire may become live before publication.
  private readonly replays = new Map<SessionId, { history: SessionWorkspaceStager; valid: boolean; wireComplete: boolean }>();
  private readonly branches = new Map<SessionId, { version: number; awaitingReplay: boolean }>();
  private receivedOrder = 0;

  /** Creates a router whose state reads and writes use the same store instance. */
  constructor(namespace: string, store: WorkspaceStoreAccess, refreshes: RouterRefreshes) {
    this.decoder = new SessionUpdateDecoder(namespace);
    this.recovery = new SessionRecoveryBuffer(namespace);
    this.store = store;
    this.refreshes = refreshes;
  }

  /** Identifies the local branch projection to which an asynchronous response belongs. */
  branchVersion(sessionId: SessionId): number {
    return this.branches.get(sessionId)?.version ?? 0;
  }

  /** Applies one transport frame's updates, committing their actions to the store once. */
  applyMany(notifications: readonly SessionUpdateNotification[]): void {
    const current = this.store.getState();
    let workspaceDraft = current;
    const refreshRuntime = new Set<SessionId>();
    const refreshBranches = new Set<SessionId>();
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
          const replay = this.replays.get(item.sessionId);
          // The server starts its live watcher after this marker, while question reservations may still delay publication.
          if (replay && item.update.sessionUpdate === "replay_complete") replay.wireComplete = true;
          const history = replay?.history ?? stager;
          const entities = history.snapshot();
          const publishedTools = history === stager ? undefined : current.sessionWorkspaces.get(item.sessionId)?.tools;
          const decoded = this.decoder.decode(item, entities, ++this.receivedOrder, publishedTools);
          if (decoded.scope === "branch") {
            // Frames from an already requested resume can still follow this notification. Drain its
            // response before accepting history again, and withdraw cards from the old branch now.
            this.branches.set(item.sessionId, { version: this.branchVersion(item.sessionId) + 1, awaitingReplay: true });
            stager.apply({ type: "transcript/cleared" });
            refreshBranches.add(item.sessionId);
          } else if (decoded.scope === "global") {
            for (const action of decoded.actions) workspaceDraft = reduceWorkspace(workspaceDraft, action);
          } else {
            for (const action of decoded.actions) {
              if (action.type !== "running/changed" && this.branches.get(item.sessionId)?.awaitingReplay) continue;
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
          // Keep historical and superseded branches quiet, but surface live failures before the history is published.
          // Older peers without replay_complete retain callback-based suppression until their replay finishes.
          if (decoded.scope === "session" && decoded.notice !== undefined && (!replay || replay.wireComplete)
            && !this.branches.get(item.sessionId)?.awaitingReplay) {
            workspaceDraft = reduceWorkspace(workspaceDraft, { type: "notice/added", notice: decoded.notice });
          }
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
    for (const sessionId of refreshBranches) {
      void this.refreshes.branch(sessionId).catch(() => console.warn(`Could not replay the active branch for Session ${sessionId}`));
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
    const replacements: [SessionId, SessionWorkspaceState][] = [];
    for (const [sessionId, stager] of stagers) {
      const committed = stager.commit();
      if (committed !== sessionWorkspace(workspaceDraft, sessionId)) replacements.push([sessionId, committed]);
    }
    return replacements.length ? withSessionWorkspaces(workspaceDraft, replacements) : workspaceDraft;
  }

  /** Routes an RPC's interaction snapshot to the same branch draft as subsequent notifications. */
  applyInteraction(sessionId: SessionId, change: InteractionChange): void {
    if (this.needsBranchReplay(sessionId)) return;
    const action = { type: "interactions/changed", change } as const;
    const replay = this.replays.get(sessionId);
    if (replay) replay.history.apply(action);
    else this.store.dispatch({ type: "session/updated", sessionId, action });
  }

  /** Reports whether navigation or an interrupted attachment still needs a complete replacement history. */
  needsBranchReplay(sessionId: SessionId): boolean {
    return this.branches.get(sessionId)?.awaitingReplay === true;
  }

  /** Publishes a complete history and its question reservations together, including on first attachment. */
  async replay(sessionId: SessionId, resume: (history: SessionWorkspaceStager) => Promise<void>): Promise<void> {
    this.recovery.requireFullReplay(sessionId);
    const branch = this.branches.get(sessionId);
    const replay = { history: new SessionWorkspaceStager(initialSessionWorkspaceState), valid: true, wireComplete: false };
    this.replays.set(sessionId, replay);
    if (branch) branch.awaitingReplay = false;
    try {
      await resume(replay.history);
      if (this.branches.get(sessionId) !== branch) return;
      if (!replay.valid) throw new Error("Session history replay was incomplete");
      const state = this.store.getState();
      if (!state.sessions.some((session) => session.sessionId === sessionId)) return;
      const replacement = { ...replay.history.commit(), running: sessionWorkspace(state, sessionId).running };
      this.store.dispatch({ type: "workspace/committed", state: withSessionWorkspaces(state, [[sessionId, replacement]]) });
    } catch (reason: unknown) {
      // A failed resume can leave a partial branch on the wire. Only a new full replay can fill the gap.
      if (this.branches.get(sessionId) === branch) {
        this.branches.set(sessionId, { version: branch?.version ?? 0, awaitingReplay: true });
      }
      this.recovery.requireFullReplay(sessionId);
      throw reason;
    } finally {
      this.replays.delete(sessionId);
    }
  }
}
