// Test entry: a turn still running.
import { mountSession } from "./sessionFixture";

mountSession(
  (agent) => {
    agent.running.add("s1");
    agent.replays.set("s1", [agent.userMessage("s1", "u1", "Fix the build"), agent.agentMessage("s1", "a1", "Looking at the logs")]);
  },
  (agent) => agent.push(agent.state("s1", "running")),
);
