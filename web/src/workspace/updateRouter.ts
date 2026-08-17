import type { SessionId, SessionUpdateNotification } from "../acp/protocol";
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

  /** Applies global updates while retaining inactive Session elicitation lifecycle state. */
  apply(notification: SessionUpdateNotification): void {
    const state = this.store.getState();
    const decoded = this.decoder.decode(notification, state, ++this.receivedOrder);
    if (decoded.scope === "session" && state.activeSessionId !== notification.sessionId) {
      for (const action of decoded.actions) {
        if (
          action.type === "mcp/elicitation-requested"
          || action.type === "mcp/elicitation-resolved"
        ) {
          this.store.dispatch(action);
        }
      }
      return;
    }
    for (const action of decoded.actions) this.store.dispatch(action);
    if (decoded.refreshRuntime) {
      void this.refreshSessionRuntime(notification.sessionId).catch((reason: unknown) => {
        this.store.dispatch({ type: "diagnostic/added", message: reason instanceof Error ? reason.message : String(reason) });
      });
    }
  }
}
