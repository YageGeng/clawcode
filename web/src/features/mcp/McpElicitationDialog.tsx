import { ExternalLink, MessageSquareWarning } from "lucide-react";
import { useState } from "react";

import type { McpElicitation } from "../../domain/model";
import { useModalFocus } from "../../shell/useModalFocus";
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
  const dialogRef = useModalFocus(
    () => void respond("cancel"),
    submitting,
    // In URL mode the external link is the first focusable element; steer the
    // initial focus to the safe Cancel button instead of opening the page.
    request.mode.type === "url"
      ? (modal) => modal.querySelector<HTMLButtonElement>('button[data-safe-focus]')
      : undefined
  );

  return (
    <div className="dialog-backdrop" role="presentation">
      <section ref={dialogRef} aria-describedby="mcp-elicitation-description" aria-labelledby="mcp-elicitation-title" aria-modal="true" className="dialog mcp-elicitation" role="dialog" tabIndex={-1}>
        <header><MessageSquareWarning size={18} aria-hidden="true" /><div><h2 id="mcp-elicitation-title">MCP 请求输入</h2><p>{request.context.serverId} · {request.context.turnId}</p></div></header>
        <p id="mcp-elicitation-description">{request.mode.message}</p>
        {request.mode.type === "url" ? (
          <a href={request.mode.url} rel="noreferrer" target="_blank"><ExternalLink size={13} /> 打开交互页面</a>
        ) : (
          <label>
            JSON response
            <textarea aria-describedby={error === undefined ? "mcp-elicitation-description" : "mcp-elicitation-error"} aria-invalid={error !== undefined} rows={7} value={content} onChange={(event) => setContent(event.target.value)} />
          </label>
        )}
        {error === undefined ? null : <div className="mcp-card__error" id="mcp-elicitation-error" role="alert">{error}</div>}
        <footer>
          <button className="secondary-button" data-safe-focus disabled={submitting} type="button" onClick={() => void respond("cancel")}>取消</button>
          <button className="danger-button" disabled={submitting} type="button" onClick={() => void respond("decline")}>拒绝</button>
          <button className="primary-button" disabled={submitting} type="button" onClick={() => void respond("accept")}>接受</button>
        </footer>
      </section>
    </div>
  );
}
