import type { RemoteConnection } from "clawcode-backend";
import { expect, test } from "nsh/test";

import { RelayTransport } from "../../src/transport/relay";

test("relay_dispatches_each_frame_in_order_before_reporting_the_native_close_reason", async () => {
  const received: string[] = [];
  const peer = {
    serve: async (deliver: (frames: string[]) => void) => {
      deliver(["one", "two"]);
      deliver(["three"]);
      return "Device access revoked";
    },
  } as RemoteConnection;
  const relay = new RelayTransport(async () => peer);
  relay.onFrame((frame) => received.push(frame));
  const ended = new Promise<void>((resolve) => relay.onClose((reason) => {
    received.push(reason!);
    resolve();
  }));
  await relay.open();
  await ended;
  expect(received).toEqual(["one", "two", "three", "Device access revoked"]);
});

test("oversized_utf8_frames_fail_before_writing_and_leave_the_remote_connection_usable", async () => {
  const sent: string[] = [];
  const relay = new RelayTransport(async () => ({
    serve: () => new Promise<string | null>(() => {}),
    send: (frame: string) => { sent.push(frame); },
    close: () => {},
  }));
  await relay.open();
  // This JSON line is shorter than 32 Mi UTF-16 units but exceeds 32 MiB on the authenticated stream.
  const frame = '"' + "图".repeat(Math.ceil((32 * 1024 * 1024) / 3)) + '"';
  expect(() => relay.send(frame)).toThrow();
  relay.send("{}");
  expect(sent).toEqual(["{}"]);
  relay.close();
});
