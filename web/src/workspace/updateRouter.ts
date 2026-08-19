import type { SessionId, SessionUpdateNotification } from "../acp/protocol";
import { sessionWorkspace } from "./state";
import type { WorkspaceAction, WorkspaceState } from "./state";
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
  private receivedOrder = 0;

  /** Creates a router whose state reads and writes use the same store instance. */
  constructor(
    namespace: string,
    store: WorkspaceStoreAccess,
    refreshSessionRuntime: (sessionId: SessionId) => Promise<void>
  ) {
    this.decoder = new SessionUpdateDecoder(namespace);
    this.store = store;
    this.refreshSessionRuntime = refreshSessionRuntime;
  }

  /** Routes every update into its owning Session, including background Sessions. */
  apply(notification: SessionUpdateNotification): void {
    const state = this.store.getState();
    const decoded = this.decoder.decode(
      notification,
      sessionWorkspace(state, notification.sessionId),
      ++this.receivedOrder
    );
    if (decoded.scope === "global") {
      for (const action of decoded.actions) this.store.dispatch(action);
    } else {
      for (const action of decoded.actions) {
        this.store.dispatch({ type: "session/updated", sessionId: notification.sessionId, action });
      }
    }
    if (decoded.refreshRuntime) {
      void this.refreshSessionRuntime(notification.sessionId).catch((reason: unknown) => {
        this.store.dispatch({
          type: "session/updated",
          sessionId: notification.sessionId,
          action: { type: "diagnostic/added", message: reason instanceof Error ? reason.message : String(reason) }
        });
      });
    }
  }
}
