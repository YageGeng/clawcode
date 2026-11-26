// Test entry: the real shell over a scripted host (no backend, no model).
import { mount } from "nsh";

import { Shell } from "../../src/shell/Shell";
import { createAppStore } from "../../src/stores/app";
import { createHostStore, LOCAL_HOST } from "../../src/stores/host";
import { createSettingsStore } from "../../src/stores/settings";
import { ScriptedAgent } from "../support/scriptedAgent";

/** Three Sessions in two workspaces; "Second" starts running once the host is up. */
function Entry() {
  const agent = new ScriptedAgent([
    { sessionId: "s1", cwd: "/work/alpha", title: "First", modifiedAtMs: "3000" },
    { sessionId: "s2", cwd: "/work/alpha", title: "Second", modifiedAtMs: "2000" },
    { sessionId: "s3", cwd: "/work/beta", title: "Third", modifiedAtMs: "1000" },
  ]);
  // A distinct host with an empty workspace catalog exercises remote-path entry in the shell.
  const remote = createHostStore({ id: "workstation", name: "Workstation", kind: "remote" }, new ScriptedAgent([]).transport);
  const store = createAppStore([createHostStore(LOCAL_HOST, agent.transport), remote]);
  const settings = createSettingsStore();
  void store.start().then(() => agent.push(agent.state("s2", "running")));
  return <Shell store={store} settings={settings} />;
}

mount(Entry);
