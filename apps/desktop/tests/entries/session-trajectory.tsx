// Test entry: Session "s1" with a conversation and its trajectory (two Runs); "s2" lists without history.
import { mountSession } from "./sessionFixture";
import { wireRun } from "../support/trajectoryRecords";

mountSession((agent) => {
  agent.replays.set("s1", [
    agent.userMessage("s1", "u1", "Fix it"),
    agent.agentMessage("s1", "a1", "Done"),
  ]);
  agent.trajectories.set("s1", [...wireRun(1, 1_700_000_000_000, 1), ...wireRun(2, 1_700_000_010_000, 10)]);
  agent.requestContents.set("a".repeat(64), "You are a careful coding agent.");
  agent.requestContents.set("b".repeat(64), JSON.stringify([{ name: "read", description: "Read a file", parameters: { type: "object" } }]));
}, undefined, [{ sessionId: "s2", cwd: "/work/alpha", title: "No history" }]);
