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
      content: [{ type: "diff", changes: [{ operation: "modify", path: "src/a.rs" }],
        patch: { format: "git_patch", text: "diff --git src/a.rs src/a.rs\n--- src/a.rs\n+++ src/a.rs\n@@ -1 +1,4 @@\n x\n+A\n+B\n+C" } }],
    }),
    agent.agentMessage("s1", "a1", "Added three lines."),
  ]);
});
