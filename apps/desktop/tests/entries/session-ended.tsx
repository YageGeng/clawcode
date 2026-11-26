// Test entry: one ended turn with commentary, two commands and an answer.
import { mountSession } from "./sessionFixture";

mountSession((agent) => {
  agent.replays.set("s1", [
    agent.userMessage("s1", "u1", "Fix the build"),
    agent.agentMessage("s1", "a1", "Let me look."),
    agent.tool("s1", { toolCallId: "t1", title: "cargo build", kind: "execute", status: "completed" }),
    agent.tool("s1", { toolCallId: "t2", title: "cargo test", kind: "execute", status: "completed" }),
    agent.agentMessage("s1", "a2", "Fixed it."),
  ]);
});
