// A paired host whose "s1" owns a running subagent. Browsing files drops the connection, so a test can take the host offline on cue.
import { mountSession } from "./sessionFixture";

mountSession((agent) => {
  agent.running.add("c1");
  const send = agent.transport.send.bind(agent.transport);
  agent.transport.send = (line) => {
    const frame = JSON.parse(line);
    if (frame.method === "_clawcode/workspace/list_dir") return agent.transport.end("offline");
    send(line);
  };
}, undefined, [
  { sessionId: "c1", cwd: "/work/alpha", title: "Lexer", meta: { ownership: { sessionId: "s1", name: "lexer", nickname: "Ada" } } },
], { id: "peer", name: "Workstation", kind: "remote" });
