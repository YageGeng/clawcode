import { RemoteConnection, RemoteHosting } from "clawcode-backend";
import { signal } from "nsh";
import { t } from "nsh/i18n";
import process from "process";

import { messageOf } from "../client/errors";
import type { NativeTransport } from "../transport/native";
import { RelayTransport } from "../transport/relay";
import type { AppStore } from "./app";
import { createHostStore } from "./host";

type Snapshot = Awaited<ReturnType<RemoteHosting["snapshot"]>>;
type Peer = Snapshot["knownHosts"][number];
type Invitation = Awaited<ReturnType<RemoteHosting["invite"]>>;

/** Protocol boundary errors are fixed, locally produced messages. Unexpected diagnostics remain available verbatim. */
const remoteFailures: Readonly<Record<string, string>> = {
  "Invalid pairing code": "invalid",
  "Pairing code has expired": "expired",
  "Pairing or device access denied": "declined",
  "Device access revoked": "revoked",
  "Remote hosting stopped": "stopped",
  "Remote device disconnected": "disconnected",
  "Remote host is unavailable": "unavailable",
  "Remote host connection timed out": "timeout",
  "Remote connection timed out": "timeout",
  "Host approval timed out": "timeout",
  "Remote connection closed": "closed",
  "Remote connection closed because its outgoing queue was full": "congested",
  "Could not deliver remote ACP frames": "delivery",
  "Cannot connect to this device's own profile": "self",
};

/** Maps connection and admission failures to actionable copy in the interface language. */
export function remoteError(error: unknown): string {
  const message = messageOf(error);
  if (/incompatible|version mismatch|application protocol|unsupported version/i.test(message)) return t("remote.incompatible");
  const key = remoteFailures[message];
  return key ? t(`remote.error.${key}`) : message;
}

/** Installation-wide identity and incoming requests, independent of the selected host and its ACP state. */
export class RemoteStore {
  private hosting?: RemoteHosting;
  private closed = false;
  private readonly relay = process.env.CLAW_DESKTOP_RELAY ?? null;
  private readonly state = signal<Snapshot | null>(null);
  private readonly invitation = signal<Invitation | null>(null);
  private readonly pairing = signal<ReturnType<RemoteHosting["beginPairing"]> | null>(null);
  private sharingChanged = false;
  private readonly working = signal<ReadonlySet<string>>(new Set());
  private readonly failure = signal<string | null>(null);
  private readonly initializationFailure = signal<string | null>(null);
  private expiration?: ReturnType<typeof setTimeout>;

  /** Observes the shared native identity and registers ACP connections in the root's host directory. */
  constructor(
    private readonly app: AppStore,
    private readonly local: Pick<NativeTransport, "localBackend">,
  ) {}

  /** Reads the identity, devices and pending approvals as last reported by the native host. */
  get snapshot() { return this.state(); }

  /** Returns only invitations that are still valid. */
  get invite() {
    const invite = this.invitation();
    return invite && this.snapshot?.enabled && this.snapshot.invitationGeneration === invite.generation ? invite : null;
  }

  /** Reads the current action's error, or a persistent native identity startup failure. */
  get error() { return this.failure() ?? this.initializationFailure(); }

  /** Remembers an opt-in across restarts, even when the local backend could not start this time. */
  get sharingRequested() { return this.snapshot?.sharingPreference === true; }

  /** Whether an action is awaiting a result. Independent actions can still run. */
  busy(action: string): boolean { return action === "pair" ? this.pairing() !== null : this.working().has(action); }

  /** Clears transient feedback when switching dialogs. */
  clearError(): void { this.failure.set(null); }

  /** Opens the identity and existing remote hosts even when the local backend failed to start. */
  async load(localReady: Promise<void>, openHosting = RemoteStore.openHosting): Promise<void> {
    try {
      const hosting = await openHosting();
      if (this.closed) {
        hosting.close();
        return;
      }
      this.hosting = hosting;
      const initial = await hosting.snapshot();
      if (this.closed) return;
      this.observe(initial);
      for (const peer of initial.knownHosts) this.connect(peer);
      void hosting.watch((snapshot) => this.observe(snapshot)).catch((error: unknown) => {
        if (!this.closed) this.initializationFailure.set(remoteError(error));
      });
      // Receiving connections requires a live local agent. The client above has no such dependency.
      await localReady;
      const backend = this.local.localBackend;
      if (!this.closed && !this.sharingChanged && this.sharingRequested && !this.snapshot?.enabled && backend) {
        await this.perform("sharing", () => hosting.start(backend, this.relay));
      }
    } catch (error: unknown) {
      if (!this.closed) this.initializationFailure.set(remoteError(error));
    }
  }

  /** Opens this installation's native identity; no listener is created until sharing is enabled. */
  private static async openHosting(): Promise<RemoteHosting> {
    if (!process.dataDir) throw new Error(t("remote.noDataDirectory"));
    return RemoteHosting.open(process.dataDir);
  }

  /** Discards a code when native admission consumes, exhausts, expires or replaces its generation. */
  private observe(snapshot: Snapshot): void {
    if (this.closed) return;
    const previous = this.snapshot?.invitationGeneration;
    this.state.set(snapshot);
    if (!snapshot.enabled || (previous != null && previous === this.invitation()?.generation && previous !== snapshot.invitationGeneration)) {
      this.clearInvitation();
    }
  }

  /** Wraps each user action with observable busy and error feedback and ignores duplicate clicks. */
  private async perform(action: string, task: (hosting: RemoteHosting) => Promise<void>): Promise<boolean> {
    if (!this.hosting || this.closed || this.busy(action)) return false;
    this.working.update((previous) => new Set(previous).add(action));
    this.clearError();
    try {
      await task(this.hosting);
      return !this.closed;
    } catch (error: unknown) {
      if (!this.closed) this.failure.set(remoteError(error));
      return false;
    } finally {
      this.working.update((previous) => {
        const next = new Set(previous);
        next.delete(action);
        return next;
      });
    }
  }

  /** Starts an authenticated connection and gives only this host a fresh ACP state on retry. */
  connect(peer: Peer, paired?: RemoteConnection): void {
    const hosting = this.hosting;
    if (!hosting || this.closed) {
      paired?.close();
      return;
    }
    const transport = new RelayTransport(() => paired ? Promise.resolve(paired) : RemoteConnection.connect(hosting, peer.id, this.relay));
    const profile = this.state;
    this.app.addHost(createHostStore({
      id: peer.id,
      kind: "remote",
      /** A reconnect may discover the remote user's new device name. */
      get name() { return profile()?.knownHosts.find((host) => host.id === peer.id)?.name ?? peer.name; },
    }, transport));
  }

  /** Submits an invitation and adopts its already authenticated ACP stream once permission is granted. */
  async pair(code: string): Promise<string | null> {
    if (!this.hosting || this.closed || this.pairing()) return null;
    this.clearError();
    let attempt: ReturnType<RemoteHosting["beginPairing"]> | undefined;
    try {
      attempt = this.hosting.beginPairing();
      this.pairing.set(attempt);
      await attempt.dial(code.trim(), this.relay);
      // Only a still-visible attempt may persist a known host and take the native connection.
      if (this.pairing() !== attempt) return null;
      const connection = attempt.accept();
      const peer = connection.peer;
      this.connect(peer, connection);
      return peer.id;
    } catch (error: unknown) {
      if (!this.closed && (!attempt || this.pairing() === attempt)) this.failure.set(remoteError(error));
      return null;
    } finally {
      if (this.pairing() === attempt) this.pairing.set(null);
    }
  }

  /** Cancels just the dismissed attempt and immediately releases the form for another one. */
  cancelPair(): void {
    const attempt = this.pairing();
    this.pairing.set(null);
    attempt?.cancel();
  }

  /** Persists opt-out before the first await; a failed write still stops the listener and reports the failure. */
  setSharing(enabled: boolean): Promise<boolean> {
    this.sharingChanged = true;
    return this.perform("sharing", async (hosting) => {
      if (enabled) {
        const backend = this.local.localBackend;
        if (!backend) throw new Error(t("remote.localUnavailable"));
        await hosting.start(backend, this.relay);
        if (this.closed) return;
      }
      try {
        hosting.setSharingPreference(enabled);
      } catch (error: unknown) {
        // Report the write failure immediately and fail closed for either direction of the toggle.
        const reason = remoteError(error);
        this.failure.set(reason);
        try {
          await hosting.stop();
        } catch (stopError: unknown) {
          throw new Error(`${reason}\n${remoteError(stopError)}`);
        }
        throw error;
      }
      if (!enabled) await hosting.stop();
    });
  }

  /** Persists the name sent to a computer when requesting access. */
  rename(name: string): Promise<boolean> {
    return this.perform("rename", (hosting) => hosting.rename(name.trim()));
  }

  /** Creates a code in memory only and removes it from the UI when the native expiry time arrives. */
  createInvitation(): Promise<boolean> {
    return this.perform("invite", async (hosting) => {
      this.clearInvitation();
      const invitation = await hosting.invite();
      if (this.closed) return;
      // QR rendering and its return cross the native watch. Drop an already retired secret instead of caching it.
      const current = await hosting.snapshot();
      if (this.closed || !current.enabled || current.invitationGeneration !== invitation.generation) return;
      const remaining = invitation.expiresAt * 1_000 - Date.now();
      if (remaining <= 0) return;
      // Only the serialized native watch authorizes display. It may invalidate this code before invite() returns.
      this.invitation.set(invitation);
      this.expiration = setTimeout(() => this.clearInvitation(), remaining);
    });
  }

  /** Drops all references to the temporary invitation and its expiry timer. */
  private clearInvitation(): void {
    clearTimeout(this.expiration);
    this.invitation.set(null);
  }

  /** Answers one specific request, so an expired request cannot approve the next device in line. */
  approve(requestId: string, allow: boolean): Promise<boolean> {
    return this.perform("approve", (hosting) => hosting.approve(requestId, allow));
  }

  /** Closes the active stream but keeps the device trusted for future connections. */
  disconnect(id: string): Promise<boolean> {
    return this.perform(id, (hosting) => hosting.disconnect(id));
  }

  /** Removes the public key from trust and closes every stream opened by that device. */
  revoke(id: string): Promise<boolean> {
    return this.perform(id, (hosting) => hosting.revoke(id));
  }

  /** Stops new work, the native watcher, and any incoming streams owned by this window. */
  close(): void {
    if (this.closed) return;
    this.closed = true;
    this.cancelPair();
    this.clearInvitation();
    this.hosting?.close();
  }
}
