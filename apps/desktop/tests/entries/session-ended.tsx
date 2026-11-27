// An ended turn with commentary, mixed tool calls and a final reply that also contains thinking.
import { mountSession } from "./sessionFixture";

mountSession((agent) => {
  agent.replays.set("s1", [
    agent.userMessage("s1", "u1", "Fix the build"),
    agent.update("s1", { sessionUpdate: "agent_thought", messageId: "a1", content: [{ type: "text", text: "Inspect the build log." }] }),
    agent.update("s1", { sessionUpdate: "agent_message", messageId: "a1", content: [
      { type: "text", text: "Let me look." }, { type: "resource_link", name: "Build log", uri: "file:///build.log" },
      { type: "text", text: "Checking the command." },
    ] }),
    agent.update("s1", { sessionUpdate: "tool_call_update", toolCallId: "t1", name: "exec_command", title: "cargo build",
      kind: "execute", rawInput: { cmd: "cargo build" }, status: "completed" }),
    agent.update("s1", { sessionUpdate: "tool_call_update", toolCallId: "t2", name: "read", kind: "read",
      rawInput: { path: "src/main.rs" }, content: [{ type: "content", content: { type: "text", text: "File contents from read" } }], status: "completed" }),
    agent.update("s1", { sessionUpdate: "tool_call_update", toolCallId: "t3", name: "mcp__custom__lookup",
      rawOutput: "Repository result", status: "completed" }),
    agent.update("s1", { sessionUpdate: "agent_thought", messageId: "a2", content: [{ type: "text", text: "The build now passes." }] }),
    agent.agentMessage("s1", "a2", "Fixed it."),
  ]);
});
