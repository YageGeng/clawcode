// The delayed first read completes after a later selection on the same host connection.
import { mountSession } from "./sessionFixture";

mountSession((agent) => {
  let pending: number | undefined;
  let lists = 0;
  const send = agent.transport.send.bind(agent.transport);
  agent.transport.send = (line) => {
    const frame = JSON.parse(line);
    const path = frame.params?.path;
    if (frame.method === "_clawcode/workspace/list_dir") {
      if (frame.params.root !== "/work/alpha") return agent.fail(frame.id, "Unexpected workspace root");
      if (path === "src" && ++lists > 1) return agent.fail(frame.id, "Repeated directory request");
      const entries = path === "" ? [
        { name: "src", path: "src", kind: "directory" },
        { name: "linked.rs", path: "linked.rs", kind: "symlink" },
        { name: "workspace.db", path: "workspace.db", kind: "file" },
      ] : ["slow.ts", "fast.ts", "binary.dat", "large.txt", "empty.txt"].map((name) =>
        ({ name, path: path + "/" + name, kind: "file" }));
      return agent.respond(frame.id, { truncated: false, entries });
    }
    if (frame.method === "_clawcode/workspace/read_file") {
      if (path === "src/slow.ts") { pending = frame.id; return; }
      if (path === "src/binary.dat") return agent.respond(frame.id, { type: "binary" });
      if (path === "src/large.txt") return agent.respond(frame.id, { type: "too_large", size: 2_000_000 });
      agent.respond(frame.id, { type: "text", text: path === "src/empty.txt" ? "" : "export const fast = true;" });
      if (pending !== undefined) { agent.respond(pending, { type: "binary" }); pending = undefined; }
      return;
    }
    send(line);
  };
  agent.replays.set("s1", [
    agent.userMessage("s1", "u", "Update the source"),
    ...["first", "second"].map((version) => agent.tool("s1", { toolCallId: version, title: "Edit source",
      kind: "edit", status: "completed", content: [{ type: "diff", path: "src/fast.ts", oldText: "", newText: version }] })),
    agent.agentMessage("s1", "a", "Done."),
  ]);
});
