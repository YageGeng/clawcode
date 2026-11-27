// Two turns edit the same path. The earlier turn also edits a second file with the same basename.
import { mountSession } from "./sessionFixture";

mountSession((agent) => {
  agent.replays.set("s1", [
    agent.userMessage("s1", "first", "Update the imports"),
    agent.tool("s1", { toolCallId: "first-edit", title: "Edit source files", kind: "edit", status: "completed", content: [
      { type: "diff", path: "src/a.rs", oldText: "use old;", newText: "use first;" },
      { type: "diff", path: "tests/a.rs", oldText: "", newText: "fn test_only() {}" },
    ] }),
    agent.tool("s1", { toolCallId: "second-edit", title: "Edit src/a.rs", kind: "edit", status: "completed", content: [
      { type: "diff", path: "src/a.rs", oldText: "use first;", newText: "use second;" },
    ] }),
    agent.agentMessage("s1", "first-answer", "Updated the imports."),
    agent.userMessage("s1", "u1", "Update the retry limits"),
    agent.tool("s1", {
      toolCallId: "t1",
      title: "Edit src/a.rs",
      kind: "edit",
      status: "completed",
      content: [{ type: "diff", changes: [{ operation: "modify", path: "src/a.rs" }],
        patch: { format: "git_patch", text: "diff --git src/a.rs src/a.rs\n--- src/a.rs\n+++ src/a.rs\n@@ -1,3 +1,5 @@\n use std::time::Duration;\n \n-const RETRIES: u8 = 1;\n+/// Retry budget per request.\n+const RETRIES: u8 = 3;\n+const MAX_RETRIES: u8 = 8;" } }],
    }),
    agent.agentMessage("s1", "a1", "Updated the retry limits."),
  ]);
});
