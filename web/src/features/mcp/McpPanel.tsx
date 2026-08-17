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
          <p>Session snapshot revision {snapshot.revision}；协议与传输显式配置，不自动回退。</p>
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
