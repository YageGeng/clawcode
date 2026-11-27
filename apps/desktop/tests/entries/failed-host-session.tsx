// A host that fails to start while the location still names one of its Sessions.
import { mount } from "nsh";

import type { SessionId } from "../../src/client/acp/protocol";
import { Shell } from "../../src/shell/Shell";
import { createAppStore } from "../../src/stores/app";
import { createHostStore, LOCAL_HOST } from "../../src/stores/host";
import { createSettingsStore } from "../../src/stores/settings";
import { MemoryTransport } from "../support/memoryTransport";

mount(() => {
  const store = createAppStore([createHostStore(LOCAL_HOST, new MemoryTransport(() => {}, "config file not found"))]);
  void store.start().then(() => store.openSession(LOCAL_HOST.id, "s1" as SessionId));
  return <Shell store={store} settings={createSettingsStore()} />;
});
