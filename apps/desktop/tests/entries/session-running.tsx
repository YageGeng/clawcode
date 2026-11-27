// Test entry: a turn still running.
import { mountSession } from "./sessionFixture";

mountSession(
  (agent) => {
    agent.running.add("s1");
    agent.replays.set("s1", [
      agent.userMessage("s1", "u1", "Fix the build"),
      agent.update("s1", { sessionUpdate: "agent_thought_chunk", messageId: "a1", content: { type: "text", text: "Check the last error." } }),
      agent.agentChunk("s1", "a1", "Current file"),
      agent.update("s1", { sessionUpdate: "agent_message_chunk", messageId: "a1",
        content: { type: "resource_link", name: "Build output", uri: "file:///build.out" } }),
      agent.agentChunk("s1", "a1", "Looking at the logs"),
    ]);
  },
  (agent) => {
    agent.push(agent.state("s1", "running"));
    // The durable snapshot revises both text spans and the linked file while the same row is mounted.
    setTimeout(() => {
      agent.push(agent.update("s1", { sessionUpdate: "agent_message", messageId: "a1", content: [
        { type: "text", text: "Final file" }, { type: "resource_link", name: "Final output", uri: "file:///final.out" },
        { type: "text", text: "Build passed." },
      ] }));
      agent.running.delete("s1");
      agent.push(agent.state("s1", "idle"));
    }, 10000);
  },
);
