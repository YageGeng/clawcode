import { Backend } from "clawcode-backend";

import { messageOf } from "../client/errors";
import type { AcpTransport } from "../client/transport";

/** How long the app's quit waits for the backend to stop, as the gpui client did. */
const QUIT_TIMEOUT_MS = 10_000;

/** ACP frames to and from the backend running in this process: no socket, no port. */
export class NativeTransport implements AcpTransport {
  private backend: Backend | undefined;
  /** Set by `close`; a backend that finishes starting after it is shut down at once. */
  private closed = false;
  private readonly frameListeners = new Set<(frame: string) => void>();
  private readonly closeListeners = new Set<(reason?: string) => void>();

  /** The local application used to serve authorized remote connections once startup finishes. */
  get localBackend(): Backend | undefined {
    return this.closed ? undefined : this.backend;
  }

  /** Starts the backend and begins delivering its frames; rejects with why it could not start. */
  async open(): Promise<void> {
    if (this.backend !== undefined) throw new Error("the backend is already started");
    const backend = await Backend.start();
    this.backend = backend;
    if (this.closed) {
      // Closed (a connect timeout, the window) while it started: it must not keep running.
      this.close();
      throw new Error("the connection closed while the backend started");
    }
    // `serve` settles when the connection ends: with its error, or null.
    backend.serve((frame) => {
      for (const listener of this.frameListeners) listener(frame);
    }).then(
      (reason) => this.ended(reason ?? undefined),
      (error: unknown) => this.ended(messageOf(error)),
    );
  }

  /** Sends one frame to the backend. */
  send(frame: string): void {
    const backend = this.localBackend;
    if (backend === undefined) throw new Error("the backend is not started");
    backend.send(frame);
  }

  /** Registers a frame listener. */
  onFrame(listener: (frame: string) => void): () => void {
    this.frameListeners.add(listener);
    return () => this.frameListeners.delete(listener);
  }

  /** Registers a close listener. */
  onClose(listener: (reason?: string) => void): () => void {
    this.closeListeners.add(listener);
    return () => this.closeListeners.delete(listener);
  }

  /** Shuts the backend down in the background; the close listeners hear of it once its frames end. */
  close(): void {
    this.closed = true;
    // Keep the native handle until the drain settles so quit can still wait synchronously.
    void this.backend?.shutdown().catch((error: unknown) => {
      console.warn(`shutting down the local backend: ${messageOf(error)}`);
    }).finally(() => { this.backend = undefined; });
  }

  /**
   * Shuts the backend down and waits for it, up to 10 s, blocking the engine: for the app's quit,
   * whose listeners NSH awaits for only 200 ms.
   */
  closeNow(): void {
    this.closed = true;
    const backend = this.backend;
    this.backend = undefined;
    if (backend !== undefined && !backend.shutdownNow(QUIT_TIMEOUT_MS)) {
      console.warn("the local backend could not confirm a complete shutdown before quit");
    }
  }

  /** Reports the end of the connection once. */
  private ended(reason: string | undefined): void {
    this.backend = undefined;
    for (const listener of this.closeListeners) listener(reason);
    this.closeListeners.clear();
  }
}
