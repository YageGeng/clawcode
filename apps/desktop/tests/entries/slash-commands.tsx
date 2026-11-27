// Opens the real composer and advertises different ACP command lists for an existing Session and a fresh one.
import { mountSession } from "./sessionFixture";

mountSession((agent) => {
  for (const [id, names] of [["s1", ["stats", "status"]], ["new-2", ["name"]]] as const) {
    agent.replays.set(id, [agent.update(id, { sessionUpdate: "available_commands_update",
      availableCommands: names.map((name) => ({
        type: "command", name, description: `Use ${name}`,
        ...(name === "name" ? { input: { type: "text", hint: "<title>" } } : {}),
      })),
    })]);
  }
  agent.onPrompt = (call) => {
    agent.push(agent.agentMessage(call.sessionId, `a-${call.id}`, `Received ${call.text}`));
    agent.respond(call.id, { stopReason: "end_turn" });
  };
});
