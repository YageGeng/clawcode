import { Backend } from "clawcode-backend";

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

  /** Starts the backend and begins delivering its frames; rejects with why it could not start. */
  async open(): Promise<void> {
    if (this.backend !== undefined) throw new Error("the backend is already started");
    const backend = await Backend.start();
    if (this.closed) {
      // Closed (a connect timeout, the window) while it started: it must not keep running.
      void backend.shutdown();
      throw new Error("the connection closed while the backend started");
    }
    this.backend = backend;
    // `serve` settles when the connection ends: with its error, or null.
    backend.serve((frame) => {
      for (const listener of this.frameListeners) listener(frame);
    }).then(
      (reason) => this.ended(reason ?? undefined),
      (error: unknown) => this.ended(error instanceof Error ? error.message : String(error)),
    );
  }

  /** Sends one frame to the backend. */
  send(frame: string): void {
    if (this.backend === undefined) throw new Error("the backend is not started");
    this.backend.send(frame);
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
    const backend = this.backend;
    this.backend = undefined;
    void backend?.shutdown();
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
      console.warn(`the backend did not stop within ${QUIT_TIMEOUT_MS} ms`);
    }
  }

  /** Reports the end of the connection once. */
  private ended(reason: string | undefined): void {
    this.backend = undefined;
    for (const listener of this.closeListeners) listener(reason);
    this.closeListeners.clear();
  }
}
