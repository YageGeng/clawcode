import { mountSession } from "./sessionFixture";

mountSession((agent) => {
  const session = "s1";
  agent.replays.set(session, [
    agent.userMessage(session, "user", "Review the work"),
    agent.update(session, { sessionUpdate: "plan_update", plan: { planId: "plan-mode", type: "markdown", content: "Build the index" } }),
    agent.update(session, { sessionUpdate: "plan_update", plan: { planId: "todos", type: "items",
      entries: [{ content: "Run checks", status: "in_progress" }] } }),
    agent.update(session, { sessionUpdate: "tool_call_update", toolCallId: "ask", status: "completed", title: "Request input",
      rawOutput: { content: { details: { type: "user_questions", request_id: "ask", questions: [
        { title: "Which environments?", options: ["Linux", "Windows"], multi_select: true },
        { title: "Add a note?" }, { title: "Send a summary?", options: ["Yes", "No"] },
      ] } } } }),
    agent.update(session, { sessionUpdate: "_clawcode/event", payload: { event: "run_end", run_id: "initial" } }),
  ]);
  agent.onPrompt = (call) => {
    if (call.text === "Reserve first question") {
      // Simulate another connected client reserving the first answer while this editor is on the second.
      agent.push(agent.update(session, { sessionUpdate: "_clawcode/event", payload: { event: "queue_changed",
        pending: { reservedQuestions: [{ requestId: "ask", index: 0 }] } } }));
      return agent.respond(call.id, { stopReason: "end_turn" });
    }
    if (call.text === "Update task progress") {
      agent.push(agent.update(session, { sessionUpdate: "plan_update", plan: { planId: "todos", type: "items",
        entries: [{ content: "Run checks", status: "completed" }] } }));
      return agent.respond(call.id, { stopReason: "end_turn" });
    }
    agent.respond(call.id, { userMessageId: "answer" });
    void agent.requestPermission({ sessionId: session, title: "Execute plan?", subject: { type: "tool_call",
      toolCall: { kind: "switch_mode", content: [{ type: "content", content: { type: "text", text: "Test both environments" } }] } },
      options: [{ optionId: "keep_planning", kind: "reject_once", name: "Keep planning" }, { optionId: "approve", kind: "allow_once", name: "Approve" }],
    }).then(() => agent.push(agent.agentMessage(session, "receipt", call.text)));
  };
});
