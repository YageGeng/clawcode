import { mount, View } from "nsh";

import { Shell } from "../../src/shell/Shell";
import { createAppStore } from "../../src/stores/app";
import { createHostStore } from "../../src/stores/host";
import { createSettingsStore } from "../../src/stores/settings";
import { Button } from "../../src/ui/button";
import { MemoryTransport } from "../support/memoryTransport";
import { ScriptedAgent } from "../support/scriptedAgent";

/** Shows the real shell while the test separately controls a connection loss and its retry outcome. */
function Entry() {
  const agent = new ScriptedAgent([{ sessionId: "A", cwd: "/work", title: "Conversation A" }]);
  agent.replays.set("A", [agent.agentMessage("A", "reply", "Saved transcript")]);
  const info = { id: "remote", name: "Workstation", kind: "remote" } as const;
  const store = createAppStore([createHostStore(info, agent.transport)]);
  const settings = createSettingsStore();
  const transport = new MemoryTransport(() => {});
  void store.start();
  return <View class="flex-1">
    <View class="flex-row">
      <Button label="Begin retry" onPress={() => {
        agent.transport.end("offline");
        store.addHost(createHostStore(info, transport));
      }} />
      <Button label="Fail retry" onPress={() => transport.end("retry failed")} />
    </View>
    <Shell store={store} settings={settings} />
  </View>;
}

mount(Entry);
