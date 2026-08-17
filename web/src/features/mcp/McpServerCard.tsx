import { CircleAlert, CircleCheck, CircleOff, RotateCw } from "lucide-react";
import { useState } from "react";

import type { McpServerStatus, McpSessionSnapshot } from "../../domain/model";
import type { WorkspaceController } from "../../workspace/controller";
import { McpCatalog } from "./McpCatalog";
import { McpOAuthAction } from "./McpOAuthAction";

export type McpServerCardProps = Readonly<{
  server: McpServerStatus;
  snapshot: McpSessionSnapshot;
  controller: WorkspaceController;
}>;

/** Renders one exact-protocol Server status, actions, and catalog projection. */
export function McpServerCard({ server, snapshot, controller }: McpServerCardProps) {
  const [reconnecting, setReconnecting] = useState(false);
  const [error, setError] = useState<string>();
  const healthy = server.state === "ready";
  const unhealthy = server.state === "failed" || server.state === "degraded";

  /** Runs one explicit reconnect without hiding a failed result. */
  const reconnect = async () => {
    setReconnecting(true);
    setError(undefined);
    try {
      await controller.reconnectMcp(server.serverId);
    } catch (reason: unknown) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setReconnecting(false);
    }
  };

  return (
    <article className="capability-card mcp-card" data-state={server.state}>
      <div className="mcp-card__heading">
        <div>
          <h3>{server.serverId}</h3>
          <p className="mcp-card__protocol">
            {server.protocol === "2025-11-25" ? "Legacy" : "Modern"} · {server.protocol} · {server.transport}
          </p>
        </div>
        <span>
          {healthy ? <CircleCheck size={14} /> : unhealthy ? <CircleAlert size={14} /> : <CircleOff size={14} />}
          {server.state}
        </span>
      </div>
      <p>{server.implementation === undefined ? "未协商实现" : `${server.implementation.name} ${server.implementation.version}`}</p>
      <p>Tools {server.counts.tools} · Prompts {server.counts.prompts} · Resources {server.counts.resources} · Templates {server.counts.resourceTemplates}</p>
      <p className="mcp-card__revision">Catalog revisions: {server.revisions.tools}/{server.revisions.prompts}/{server.revisions.resources}/{server.revisions.resourceTemplates}</p>
      {server.failure === undefined ? null : <div className="mcp-card__error">{server.failure.stage}: {server.failure.message}</div>}
      {error === undefined ? null : <div className="mcp-card__error">{error}</div>}
      <McpOAuthAction controller={controller} server={server} />
      {server.state === "disabled" ? null : (
        <button disabled={reconnecting} type="button" onClick={() => void reconnect()}>
          <RotateCw size={13} /> {reconnecting ? "Reconnecting…" : "Reconnect"}
        </button>
      )}
      <McpCatalog controller={controller} serverId={server.serverId} snapshot={snapshot} />
    </article>
  );
}
