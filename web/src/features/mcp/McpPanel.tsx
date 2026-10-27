import { PlugZap } from "lucide-react";

import type { McpSessionSnapshot } from "../../domain/model";
import type { WorkspaceController } from "../../workspace/controller";
import { McpServerCard } from "./McpServerCard";

export type McpPanelProps = Readonly<{
  snapshot: McpSessionSnapshot;
  controller: WorkspaceController;
}>;

/** Renders the Session MCP overview while delegating each Server interaction. */
export function McpPanel({ snapshot, controller }: McpPanelProps) {
  return (
    <section className="capability-page">
      <header className="capability-page__intro">
        <PlugZap size={22} />
        <div>
          <h2>MCP</h2>
          <p>Session snapshot revision {snapshot.revision}；各 Server 独立协商协议，目录由当前 Session 提供。</p>
        </div>
      </header>
      <div className="capability-grid">
        {snapshot.servers.length === 0
          ? <div className="capability-empty">当前会话没有 MCP Server。</div>
          : snapshot.servers.map((server) => (
              <McpServerCard
                controller={controller}
                key={server.serverId}
                server={server}
                snapshot={snapshot}
              />
            ))}
      </div>
    </section>
  );
}
