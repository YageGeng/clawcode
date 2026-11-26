import type { RemoteConnection } from "clawcode-backend";
import { expect, test } from "nsh/test";

import { RelayTransport } from "./relay";

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
