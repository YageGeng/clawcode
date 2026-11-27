import type { RemoteConnection } from "clawcode-backend";
import { Buffer } from "buffer";
import { t } from "nsh/i18n";

import type { AcpTransport } from "../client/transport";
import { messageOf } from "../client/errors";

/** Maximum JSON line the authenticated remote stream accepts (crates/remote/src/session.rs). */
const maxFrameBytes = 32 * 1024 * 1024;

/** The authenticated frame stream the relay transport uses after pairing or dialing. */
type FrameStream = Pick<RemoteConnection, "send" | "serve" | "close">;

/** One authenticated ACP link, obtained by dialing a known computer or completing pairing. */
export class RelayTransport implements AcpTransport {
  private connection?: FrameStream;
  private closed = false;
  private frames = new Set<(frame: string) => void>();
  private endings = new Set<(reason?: string) => void>();

  /** Defers the dial until the ACP frame listener is installed. */
  constructor(private readonly dial: () => Promise<FrameStream>) {}

  /** Starts delivering frames, handling a connect timeout or window close during the dial. */
  async open(): Promise<void> {
    const peer = await this.dial();
    if (this.closed) {
      peer.close();
      throw new Error("The remote connection was closed");
    }
    this.connection = peer;
    void peer.serve((frames) => {
      // Returning from this callback acknowledges the whole batch to the native reader.
      for (const frame of frames) {
        for (const listener of this.frames) listener(frame);
      }
    }).then(
      (reason) => this.end(reason ?? undefined),
      (error: unknown) => this.end(messageOf(error)),
    );
  }

  /** Sends an ACP message on the authenticated stream. */
  send(frame: string): void {
    if (!this.connection) throw new Error("The remote computer is offline");
    // Match the peer's 32 MiB line limit. Reject locally before an oversized prompt tears down a healthy stream.
    if (frame.length > maxFrameBytes / 3 && Buffer.byteLength(frame, "utf8") > maxFrameBytes) {
      throw new Error(t("attachments.remoteSize"));
    }
    this.connection.send(frame);
  }

  /** Registers an ACP frame subscriber. */
  onFrame(listener: (frame: string) => void): () => void {
    this.frames.add(listener);
    return () => this.frames.delete(listener);
  }

  /** Registers a subscriber for remote closure, including host-side revocation. */
  onClose(listener: (reason?: string) => void): () => void {
    this.endings.add(listener);
    return () => this.endings.delete(listener);
  }

  /** Releases this peer, leaving other hosts and local hosting alone. */
  close(): void {
    this.closed = true;
    this.connection?.close();
    this.end();
  }

  /** Delivers the terminal event once and releases all callbacks. */
  private end(reason?: string): void {
    this.connection = undefined;
    for (const listener of this.endings) listener(reason);
    this.endings.clear();
    this.frames.clear();
  }
}
