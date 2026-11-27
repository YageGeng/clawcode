// Live ACP replacements for the same file, driven by prompts after the user opens and scrolls its diff.
import { mountSession } from "./sessionFixture";

mountSession((agent) => {
  const path = "review.ts";
  const lines = Array.from({ length: 180 }, (_, i) => `+const line${i} = ${i};`);
  const patch = { type: "diff", changes: [{ operation: "modify", path }],
    patch: { format: "git_patch", text: [`diff --git ${path} ${path}`, `--- ${path}`, `+++ ${path}`, "@@ -0,0 +1,180 @@", ...lines].join("\n") } };
  const reorder = { type: "diff", path, oldText: "alpha\nbeta\n", newText: "beta\nalpha\n" };
  /** Creates an authoritative tool update, including explicit content withdrawal. */
  const update = (toolCallId: string, content: unknown[]) => agent.tool("s1", { toolCallId, title: "Edit file", kind: "edit", status: "completed", content });
  agent.replays.set("s1", [
    agent.userMessage("s1", "u", "Make changes"),
    update("first", [reorder]), update("second", [patch]), update("third", [{ type: "diff", path, oldText: "", newText: "last" }]),
    agent.agentMessage("s1", "answer", "Done."),
  ]);
  agent.onPrompt = ({ id, text }) => {
    const replacement = text === "resend" ? update("second", [patch]) : update(text, []);
    agent.push(replacement);
    agent.push(agent.agentMessage("s1", "ack", `Applied ${text}`));
    agent.respond(id, { stopReason: "end_turn" });
  };
});
