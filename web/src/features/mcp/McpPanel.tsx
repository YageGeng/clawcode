import { CircleAlert, CircleCheck, CircleOff, PlugZap } from "lucide-react";

import type { McpServerInfo } from "../../domain/model";

export type McpPanelProps = Readonly<{
  servers: readonly McpServerInfo[];
}>;

export function McpPanel({ servers }: McpPanelProps) {
  return (
    <section className="capability-page">
      <header className="capability-page__intro"><PlugZap size={22} /><div><h2>MCP</h2><p>连接状态与工具目录来自当前 Session；配置只读，修改 TOML 后重启生效。</p></div></header>
      <div className="capability-grid">
        {servers.length === 0 ? <div className="capability-empty">当前会话没有 MCP Server。</div> : servers.map((server) => (
          <article className="capability-card mcp-card" data-state={server.state} key={server.name}>
            <div className="mcp-card__heading"><h3>{server.name}</h3><span>{server.state === "connected" ? <CircleCheck size={14} /> : server.state === "failed" ? <CircleAlert size={14} /> : <CircleOff size={14} />}{server.state}</span></div>
            {server.error === undefined ? null : <div className="mcp-card__error">{server.error}</div>}
            <div className="mcp-tools">
              {server.tools.length === 0 ? <p>未导出工具</p> : server.tools.map((tool) => <details key={tool.name}><summary>{tool.name}</summary>{tool.description === undefined ? null : <p>{tool.description}</p>}<pre>{JSON.stringify(tool.inputSchema, null, 2)}</pre></details>)}
            </div>
          </article>
        ))}
      </div>
    </section>
  );
}
