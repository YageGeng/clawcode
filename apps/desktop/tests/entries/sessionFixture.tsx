// Test support: mounts the real shell over a scripted host with Session "s1" open.
import { mount } from "nsh";

import type { SessionId } from "../../src/client/acp/protocol";
import { Shell } from "../../src/shell/Shell";
import { createAppStore } from "../../src/stores/app";
import { createHostStore, LOCAL_HOST } from "../../src/stores/host";
import { createSettingsStore } from "../../src/stores/settings";
import { ScriptedAgent } from "../support/scriptedAgent";

/** Mounts the shell; `setup` scripts the agent before start, `opened` runs once "s1" is open. */
export function mountSession(setup: (agent: ScriptedAgent) => void, opened?: (agent: ScriptedAgent) => void): void {
  mount(() => {
    const agent = new ScriptedAgent([{ sessionId: "s1", cwd: "/work/alpha", title: "Fix the build" }]);
    setup(agent);
    const store = createAppStore([createHostStore(LOCAL_HOST, agent.transport)]);
    void store.start().then(async () => {
      store.openSession(LOCAL_HOST.id, "s1" as SessionId);
      opened?.(agent);
    });
    return <Shell store={store} settings={createSettingsStore()} />;
  });
}
