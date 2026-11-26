// Test support: an in-memory ACP transport and a scripted peer for it.
import type { SessionInfo } from "../../src/client/acp/protocol";
import type { AcpTransport } from "../../src/client/transport";

/** An in-memory transport whose peer is a function answering each sent frame. */
export class MemoryTransport implements AcpTransport {
  private readonly frameListeners = new Set<(frame: string) => void>();
  private readonly closeListeners = new Set<(reason?: string) => void>();
  private readonly peer: (frame: string, transport: MemoryTransport) => void;
  private readonly openError: string | undefined;

  /** Creates a transport that hands every sent frame to `peer`; with `openError`, `open` rejects with it. */
  constructor(peer: (frame: string, transport: MemoryTransport) => void, openError?: string) {
    this.peer = peer;
    this.openError = openError;
  }

  /** Opens immediately, or rejects with the configured error. */
  open(): Promise<void> {
    return this.openError === undefined ? Promise.resolve() : Promise.reject(new Error(this.openError));
  }

  /** Delivers the frame to the peer. */
  send(frame: string): void {
    this.peer(frame, this);
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

  /** Ends the stream without a reason. */
  close(): void {
    this.end();
  }

  /** Delivers one frame from the peer. */
  deliver(frame: string): void {
    for (const listener of this.frameListeners) listener(frame);
  }

  /** Ends the stream from the peer's side. */
  end(reason?: string): void {
    for (const listener of this.closeListeners) listener(reason);
  }
}

/** A peer that answers `initialize` and `session/list` (with `sessions`) and ignores everything else. */
export function acpPeer(sessions: SessionInfo[]): (frame: string, transport: MemoryTransport) => void {
  return (frame, transport) => {
    const request = JSON.parse(frame) as { id?: number; method?: string };
    if (request.id === undefined) return;
    const result =
      request.method === "initialize" ? { protocolVersion: 2 } : request.method === "session/list" ? { sessions } : undefined;
    if (result !== undefined) transport.deliver(JSON.stringify({ jsonrpc: "2.0", id: request.id, result }));
  };
}
