import { useState } from "react";

import type { McpResourceInfo, McpResourceResult } from "../../domain/model";
import type { WorkspaceController } from "../../workspace/controller";

export type McpResourceEntryProps = Readonly<{
  resource: McpResourceInfo;
  controller: WorkspaceController;
}>;

/** Reads one remote Resource on demand without automatic prompt injection. */
export function McpResourceEntry({ resource, controller }: McpResourceEntryProps) {
  const [result, setResult] = useState<McpResourceResult>();
  const [error, setError] = useState<string>();

  /** Reads and preserves every typed Resource content block. */
  const read = async () => {
    setError(undefined);
    try {
      setResult(await controller.readMcpResource(resource.reference.serverId, resource.reference.remoteUri));
    } catch (reason: unknown) {
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  };

  return (
    <details>
      <summary><span>Resource</span>{resource.title ?? resource.name}</summary>
      <p>{resource.reference.remoteUri}</p>
      {resource.mimeType === undefined ? null : <p>{resource.mimeType}</p>}
      <div className="mcp-catalog__form"><button type="button" onClick={() => void read()}>Read Resource</button></div>
      {error === undefined ? null : <div className="mcp-card__error">{error}</div>}
      {result === undefined ? null : <pre>{JSON.stringify(result, null, 2)}</pre>}
    </details>
  );
}
