import { signal } from "nsh";
import type { Accessor } from "nsh";

import type { SessionId } from "../client/acp/protocol";
import { messageOf } from "../client/errors";
import type { HostStore } from "./host";
import { NavigationHistory } from "./navigation";
import type { AppLocation } from "./navigation";

/** The app's shared state: its hosts and where the user is; its methods are the only writers. */
export type AppStore = Readonly<{
  hosts: Accessor<readonly HostStore[]>;
  currentHost: Accessor<HostStore>;
  location: Accessor<AppLocation>;
  canGoBack: Accessor<boolean>;
  canGoForward: Accessor<boolean>;
  /** Starts a new or reconnected host, restoring its open Session on reconnect. */
  addHost: (host: HostStore) => void;
  /** Shows a host with no Session open. */
  selectHost: (hostId: string) => void;
  /** Opens a Session; the sidebar follows to its host (spec §4.4). */
  openSession: (hostId: string, sessionId: SessionId) => void;
  /** Creates a Session in `cwd` on the host, shows it and sends `text` as its first prompt; rejects when no Session could be created. */
  startSession: (hostId: string, cwd: string, text: string) => Promise<void>;
  back: () => void;
  forward: () => void;
  /** Starts every host independently; one failing does not stop the others. */
  start: () => Promise<void>;
  /** Stops every host. */
  stop: () => void;
}>;

/** Creates the app store; the first host is the initial location. */
export function createAppStore(initialHosts: readonly HostStore[]): AppStore {
  const first = initialHosts[0];
  if (first === undefined) throw new Error("the app needs at least one host");
  const hosts = signal<readonly HostStore[]>(initialHosts);
  const history = new NavigationHistory({ hostId: first.info.id, sessionId: null });
  // The history is plain data; this signal re-publishes its state after each move.
  const position = signal({ location: history.current, canGoBack: false, canGoForward: false });
  /** Copies the history's current state into `position`. */
  const publish = () =>
    position.set({ location: history.current, canGoBack: history.canGoBack, canGoForward: history.canGoForward });
  /** The host with `hostId`, if the app has it. */
  const find = (hostId: string) => hosts().find((host) => host.info.id === hostId);
  /** Attaches a visible Session to its current ACP connection; offline locations keep their cached transcript. */
  const attachCurrent = () => {
    const { hostId, sessionId } = history.current;
    const host = find(hostId);
    if (sessionId && host?.phase().kind === "ready") {
      void host.openSession(sessionId).catch((error: unknown) => console.warn(`open ${sessionId} on ${hostId}: ${messageOf(error)}`));
    }
  };
  /** Moves to `location` when its host exists; warns and stays otherwise. */
  const go = (location: AppLocation) => {
    if (find(location.hostId) === undefined) {
      console.warn(`no host ${location.hostId}; staying where the app is`);
      return;
    }
    history.push(location);
    publish();
  };
  return {
    hosts,
    currentHost: () => find(position().location.hostId) ?? first,
    location: () => position().location,
    canGoBack: () => position().canGoBack,
    canGoForward: () => position().canGoForward,
    addHost: (host) => {
      const previous = find(host.info.id);
      previous?.stop();
      // Seed the retry's directory and transcripts before publishing it, so the mounted composer keeps its draft.
      const started = host.start(previous);
      hosts.update((list) => previous
        ? list.map((item) => item.info.id === host.info.id ? host : item)
        : [...list, host]);
      // Keep the navigation entry across retries. Replay only if this connection and location are still current.
      void started.then(() => {
        if (history.current.hostId === host.info.id && find(host.info.id) === host) attachCurrent();
      });
    },
    selectHost: (hostId) => go({ hostId, sessionId: null }),
    openSession: (hostId, sessionId) => {
      go({ hostId, sessionId });
      if (history.current.hostId === hostId) attachCurrent();
    },
    startSession: async (hostId, cwd, text) => {
      const host = find(hostId);
      if (host === undefined) throw new Error(`no host ${hostId}`);
      // `newSession` already replays the new Session, so this navigates without `openSession`'s second replay.
      const sessionId = await host.newSession(cwd);
      go({ hostId, sessionId });
      await host.prompt(sessionId, text);
    },
    back: () => {
      if (history.back() !== undefined) {
        publish();
        attachCurrent();
      }
    },
    forward: () => {
      if (history.forward() !== undefined) {
        publish();
        attachCurrent();
      }
    },
    start: async () => {
      await Promise.all(hosts().map((host) => host.start()));
    },
    stop: () => {
      for (const host of hosts()) host.stop();
    },
  };
}
