// A replay with a mixed-media answer, a user shell command and a new Run opened by a question answer.
import { mountSession } from "./sessionFixture";

mountSession((agent) => {
  const id = "s1";
  const initial = [
    agent.userMessage(id, "user", "Show the report"),
    agent.update(id, { sessionUpdate: "agent_message", messageId: "first", content: [
      { type: "text", text: "Before the file." }, { type: "resource_link", name: "Report file", uri: "file:///report" },
      { type: "text", text: "After the file." },
    ] }),
  ];
  const shell = agent.userMessage(id, "shell", "!pwd");
  const meta = shell.update._meta as { clawcode: Record<string, unknown> };
  agent.replays.set(id, [
    ...initial,
    { ...shell, update: { ...shell.update, _meta: { clawcode: { ...meta.clawcode, origin: "user_bash" } } } },
    agent.tool(id, { toolCallId: "shell", title: "pwd", kind: "execute", status: "completed" }),
    agent.update(id, { sessionUpdate: "_clawcode/event", payload: { event: "run_start", run_id: "followup" } }),
    agent.update(id, { sessionUpdate: "_clawcode/event", payload: { event: "message_end", message: {
      message_id: "answers", content: { type: "extension", extension: { extension_id: "clawcode", custom_type: "question_reply",
        details: { items: [{ request_id: "ask", index: 0, question: "Which format?", answer: "Plain text" },
          { request_id: "ask", index: 1, question: "Add a chart?" }] } } },
    } } }),
    agent.agentMessage(id, "followup", "Format confirmed."),
  ]);
});
