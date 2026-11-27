import type { Backend, RemoteHosting } from "clawcode-backend";
import { expect, test } from "nsh/test";

import { createAppStore } from "../../src/stores/app";
import { createHostStore, LOCAL_HOST } from "../../src/stores/host";
import { RemoteStore } from "../../src/stores/remote";
import { createSettingsStore, DEFAULT_SETTINGS } from "../../src/stores/settings";
import { acpPeer, MemoryTransport } from "../support/memoryTransport";

type Snapshot = Awaited<ReturnType<RemoteHosting["snapshot"]>>;

/** Controls when a native operation delivers its result to the UI scheduler. */
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<T>((done, fail) => { resolve = done; reject = fail; });
  return { promise, resolve, reject };
}

/** Drives the public store with independently timed native calls and watch notifications. */
function controls(overrides: Partial<RemoteHosting>, saved = false, localBackend?: Backend) {
  const app = createAppStore([createHostStore(LOCAL_HOST, new MemoryTransport(acpPeer([])))]);
  const remote = new RemoteStore(app, { localBackend });
  let current: Snapshot = {
    id: "local", name: "Local", enabled: saved, sharingPreference: saved, invitationGeneration: null,
    trusted: [], online: [], pending: [], knownHosts: [],
  };
  let listener: ((state: Snapshot) => void) | undefined;
  const hosting = {
    snapshot: async () => current,
    watch: (watcher: (state: Snapshot) => void) => {
      listener = watcher;
      return Promise.resolve();
    },
    close: () => {},
    ...overrides,
  } as RemoteHosting;
  return {
    app,
    remote,
    load: () => remote.load(Promise.resolve(), async () => hosting),
    publish: (change: Partial<Snapshot>) => {
      current = { ...current, ...change };
      listener?.(current);
    },
  };
}

test("disabling_sharing_saves_before_stop_and_is_independent_of_appearance_edits_and_quit", async () => {
  const drain = deferred<void>();
  const events: string[] = [];
  const fixture = controls({
    setSharingPreference: (enabled) => { events.push(`saved ${enabled}`); },
    stop: () => { events.push("draining"); return drain.promise; },
  }, true);
  await fixture.load();
  const settings = createSettingsStore();
  settings.update({ language: "en" });
  const result = fixture.remote.setSharing(false);
  expect(events).toEqual(["saved false", "draining"]);
  settings.update({ appearance: "dark" });
  fixture.remote.close();
  drain.resolve();
  await result;
  expect(settings.settings()).toEqual({ ...DEFAULT_SETTINGS, appearance: "dark", language: "en" });
});

test("a_rejected_preference_write_reports_the_error_and_stops_in_both_directions", async () => {
  for (const enabled of [false, true]) {
    const drain = deferred<void>();
    const events: string[] = [];
    const fixture = controls({
      start: async () => { events.push("started"); },
      setSharingPreference: () => { throw new Error("write refused"); },
      stop: () => { events.push("draining"); return drain.promise; },
    }, !enabled, {} as Backend);
    await fixture.load();
    const result = fixture.remote.setSharing(enabled);
    await Promise.resolve();
    expect(events).toEqual(enabled ? ["started", "draining"] : ["draining"]);
    expect(fixture.remote.error).toBe("write refused");
    drain.resolve();
    expect(await result).toBe(false);
    expect(fixture.remote.sharingRequested).toBe(!enabled);
    fixture.remote.close();
  }
});

test("dismissal_blocks_late_pairing_adoption_and_immediately_permits_another_attempt", async () => {
  const first = deferred<void>();
  const second = deferred<void>();
  const calls: string[] = [];
  const attempts = [first, second];
  const fixture = controls({
    beginPairing: () => {
      const attempt = attempts.shift()!;
      const name = attempt === first ? "first" : "second";
      return {
        dial: () => attempt.promise,
        cancel: () => { calls.push(`cancel ${name}`); },
        accept: () => { calls.push(`accept ${name}`); throw new Error("adoption failed"); },
        dispose: () => {},
        [Symbol.dispose]: () => {},
      };
    },
  });
  await fixture.load();
  const abandoned = fixture.remote.pair("first code");
  fixture.remote.cancelPair();
  expect(fixture.remote.busy("pair")).toBe(false);
  const current = fixture.remote.pair("second code");
  first.resolve();
  expect(await abandoned).toBe(null);
  expect(fixture.remote.busy("pair")).toBe(true);
  expect(fixture.remote.error).toBe(null);
  expect(fixture.app.hosts().length).toBe(1);
  expect(calls).toEqual(["cancel first"]);
  second.reject(new Error("second attempt refused"));
  expect(await current).toBe(null);
  expect(fixture.remote.error).toBe("second attempt refused");
  fixture.remote.close();
});

test("invitation_view_follows_native_exhaustion_even_when_the_qr_result_arrives_late", async () => {
  const code = { code: "example", generation: "attempt-1", expiresAt: Math.ceil(Date.now() / 1000) + 60, qrUri: "data:example" };
  for (const exhaustedBeforeReturn of [true, false]) {
    const create = deferred<typeof code>();
    const fixture = controls({ invite: () => create.promise }, true);
    await fixture.load();
    const result = fixture.remote.createInvitation();
    fixture.publish({ invitationGeneration: code.generation });
    if (exhaustedBeforeReturn) fixture.publish({ invitationGeneration: null });
    create.resolve(code);
    expect(await result).toBe(true);
    if (!exhaustedBeforeReturn) {
      expect(fixture.remote.invite?.code).toBe(code.code);
      fixture.publish({ invitationGeneration: null });
    }
    expect(fixture.remote.invite).toBe(null);
    fixture.remote.close();
  }
});
