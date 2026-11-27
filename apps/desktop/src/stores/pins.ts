import { signal, Storage } from "nsh";

import type { SessionId } from "../client/acp/protocol";

/** The `Storage` key pinned Sessions live under: `{ [hostId]: SessionId[] }` as JSON. */
const STORAGE_KEY = "pins";

/** Sessions the user pinned to the top of the sidebar, per host. The host has no pin, so they stay on this device. */
export type PinStore = Readonly<{
  /** The pinned Sessions of `hostId`, in the order they were pinned. */
  pinned: (hostId: string) => ReadonlySet<SessionId>;
  /** Pins a Session, or unpins it when it already is, and saves the change. */
  toggle: (hostId: string, sessionId: SessionId) => void;
  /** Reads the saved pins; unreadable JSON starts empty. */
  load: () => Promise<void>;
}>;

/** Keeps only string ids per host from saved JSON. */
function parse(saved: string | null): ReadonlyMap<string, ReadonlySet<SessionId>> {
  try {
    const value: unknown = JSON.parse(saved ?? "{}");
    if (typeof value !== "object" || value === null) return new Map();
    return new Map(Object.entries(value).map(([host, ids]) =>
      [host, new Set(Array.isArray(ids) ? ids.filter((id): id is SessionId => typeof id === "string") : [])]));
  } catch {
    return new Map();
  }
}

/** Creates the pin store, empty until `load`. */
export function createPinStore(): PinStore {
  const pins = signal<ReadonlyMap<string, ReadonlySet<SessionId>>>(new Map());
  const empty: ReadonlySet<SessionId> = new Set();
  return {
    pinned: (hostId) => pins().get(hostId) ?? empty,
    toggle: (hostId, sessionId) => {
      const next = new Map(pins());
      const ids = new Set(next.get(hostId));
      if (!ids.delete(sessionId)) ids.add(sessionId);
      next.set(hostId, ids);
      pins.set(next);
      void Storage.setItem(STORAGE_KEY, JSON.stringify(Object.fromEntries([...next].map(([host, set]) => [host, [...set]]))));
    },
    load: async () => pins.set(parse(await Storage.getItem(STORAGE_KEY))),
  };
}
