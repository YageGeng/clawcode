// Test entry: Session "s1" with two finished turns, for Edit and Fork on its messages.
import { mountSession } from "./sessionFixture";

mountSession((agent) => {
  agent.replays.set("s1", [
    agent.userMessage("s1", "u1", "First question"),
    agent.agentMessage("s1", "a1", "First answer"),
    agent.userMessage("s1", "u2", "Second question"),
    agent.agentMessage("s1", "a2", "Second answer"),
  ]);
});
