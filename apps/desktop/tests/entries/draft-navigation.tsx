import { mount, signal, View } from "nsh";

import { Shell } from "../../src/shell/Shell";
import { createAppStore } from "../../src/stores/app";
import { createHostStore, LOCAL_HOST } from "../../src/stores/host";
import { createSettingsStore } from "../../src/stores/settings";
import { Button } from "../../src/ui/button";
import { ScriptedAgent } from "../support/scriptedAgent";

/** Holds the first Session creation until released so navigation can overtake it deterministically. */
function Entry() {
  const agent = new ScriptedAgent([{ sessionId: "s1", cwd: "/work/alpha", title: "Earlier session" }]);
  for (const [id, command, policy] of [["new-2", "first", "allow"], ["new-3", "second", "host"]] as const) {
    agent.replays.set(id, [agent.update(id, {
      sessionUpdate: "available_commands_update",
      availableCommands: [{ type: "command", name: command, description: command }],
    }), agent.update(id, {
      sessionUpdate: "config_option_update",
      configOptions: [{ configId: "file_approval", type: "select", name: "File approvals", currentValue: policy,
        options: [{ value: "host", name: "Host default" }, { value: "allow", name: "Allow external writes" }] }],
    })]);
  }
  agent.onPrompt = (call) => {
    const frames = [agent.userMessage(call.sessionId, "user", call.text),
      agent.agentMessage(call.sessionId, "reply", `Received on ${call.sessionId}`)];
    // Sidebar navigation resumes from the same durable history as the live prompt.
    agent.replays.set(call.sessionId, frames);
    for (const frame of frames) agent.push(frame);
    agent.respond(call.id, { stopReason: "end_turn" });
  };
  const host = createHostStore(LOCAL_HOST, agent.transport);
  const waiting = signal<{ line: string; id: number } | null>(null);
  const send = agent.transport.send.bind(agent.transport);
  let held = false;
  // Hold the first attach after creation so the test can release it or fail it while the user keeps editing.
  agent.transport.send = (line) => {
    const frame = JSON.parse(line);
    if (!held && frame.method === "session/resume" && frame.params.sessionId === "new-2") {
      held = true;
      waiting.set({ line, id: frame.id });
    } else send(line);
  };
  const store = createAppStore([host]);
  void store.start();
  return <View class="flex-1">
    <If when={waiting()}>{(request) => <View class="flex-row">
      <Button label="Release first creation" onPress={() => { send(request().line); waiting.set(null); }} />
      <Button label="Fail first attachment" onPress={() => { agent.fail(request().id, "First attachment failed"); waiting.set(null); }} />
    </View>}</If>
    <Shell store={store} settings={createSettingsStore()} />
  </View>;
}

mount(Entry);
