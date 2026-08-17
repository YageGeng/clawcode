import { ExternalLink, MessageSquareWarning } from "lucide-react";
import { useState } from "react";

import type { McpElicitation } from "../../domain/model";
import type { WorkspaceController } from "../../workspace/controller";

export type McpElicitationDialogProps = Readonly<{
  request: McpElicitation;
  controller: WorkspaceController;
}>;

/** Renders one blocking Turn-scoped MCP interaction without client FS callbacks. */
export function McpElicitationDialog({ request, controller }: McpElicitationDialogProps) {
  const [content, setContent] = useState("{}");
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string>();

  /** Sends one typed disposition and validates structured form JSON before acceptance. */
  const respond = async (action: "accept" | "decline" | "cancel") => {
    setSubmitting(true);
    setError(undefined);
    try {
      const parsed = action === "accept" && request.mode.type === "form" ? JSON.parse(content) as unknown : undefined;
      await controller.respondMcpElicitation(request, action, parsed);
    } catch (reason: unknown) {
      setError(reason instanceof Error ? reason.message : String(reason));
      setSubmitting(false);
    }
  };

  return (
    <div className="dialog-backdrop" role="presentation">
      <section aria-modal="true" className="dialog mcp-elicitation" role="dialog">
        <header><MessageSquareWarning size={18} /><div><h2>MCP 请求输入</h2><p>{request.context.serverId} · {request.context.turnId}</p></div></header>
        <p>{request.mode.message}</p>
        {request.mode.type === "url" ? (
          <a href={request.mode.url} rel="noreferrer" target="_blank"><ExternalLink size={13} /> 打开交互页面</a>
        ) : (
          <label>
            JSON response
            <textarea rows={7} value={content} onChange={(event) => setContent(event.target.value)} />
          </label>
        )}
        {error === undefined ? null : <div className="mcp-card__error">{error}</div>}
        <footer>
          <button disabled={submitting} type="button" onClick={() => void respond("cancel")}>Cancel</button>
          <button disabled={submitting} type="button" onClick={() => void respond("decline")}>Decline</button>
          <button disabled={submitting} type="button" onClick={() => void respond("accept")}>Accept</button>
        </footer>
      </section>
    </div>
  );
}
