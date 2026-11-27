// "s1" was forked from "s0" and owns a running subagent ("Ada") and a finished one ("Bo").
import { mountSession } from "./sessionFixture";

mountSession((agent) => {
  agent.sessions[0] = { ...agent.sessions[0]!, meta: { parentSessionId: "s0" } };
  agent.running.add("c1");
  agent.replays.set("s1", [agent.userMessage("s1", "u", "Split the parser"), agent.agentMessage("s1", "a", "Started two helpers.")]);
}, undefined, [
  { sessionId: "s0", cwd: "/work/alpha", title: "Original plan" },
  { sessionId: "c1", cwd: "/work/alpha", title: "Lexer", modifiedAtMs: "2", meta: { ownership: { sessionId: "s1", name: "lexer", nickname: "Ada" } } },
  { sessionId: "c2", cwd: "/work/alpha", title: "Grammar", modifiedAtMs: "1", meta: { ownership: { sessionId: "s1", name: "grammar", nickname: "Bo" } } },
]);
