// Test entry: an ended turn that edited a file.
import { mountSession } from "./sessionFixture";

mountSession((agent) => {
  agent.replays.set("s1", [
    agent.userMessage("s1", "u1", "Add the lines"),
    agent.tool("s1", {
      toolCallId: "t1",
      title: "Edit src/a.rs",
      kind: "edit",
      status: "completed",
      content: [{ type: "diff", path: "src/a.rs", oldText: "x", newText: "x\nA\nB\nC" }],
    }),
    agent.agentMessage("s1", "a1", "Added three lines."),
  ]);
});
