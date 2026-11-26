/** One bidirectional stream of ACP JSON-RPC frames, one JSON text per frame. */
export interface AcpTransport {
  /** Opens the stream; rejects when it cannot. */
  open(): Promise<void>;
  /** Sends one frame; throws when the stream is not open. */
  send(frame: string): void;
  /** Calls `listener` with every frame received, in order; returns the unsubscribe. */
  onFrame(listener: (frame: string) => void): () => void;
  /** Calls `listener` once when the stream ends, with the reason if it failed. */
  onClose(listener: (reason?: string) => void): () => void;
  /** Ends the stream. */
  close(): void;
}
