import { ExternalLink, KeyRound } from "lucide-react";
import { useState } from "react";

import type { McpServerStatus } from "../../domain/model";
import type { WorkspaceController } from "../../workspace/controller";

export type McpOAuthActionProps = Readonly<{
  server: McpServerStatus;
  controller: WorkspaceController;
}>;

/** Renders the browser handoff and callback continuation for pending OAuth. */
export function McpOAuthAction({ server, controller }: McpOAuthActionProps) {
  const [responseUri, setResponseUri] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string>();
  const authorizationUrl = server.oauth.authorizationUrl;
  if (server.oauth.state !== "unauthenticated" || authorizationUrl === undefined) return null;

  /** Submits the complete callback URI to the retained PKCE state machine. */
  const continueAuthorization = async () => {
    if (responseUri.trim().length === 0) return;
    setSubmitting(true);
    setError(undefined);
    try {
      await controller.continueMcpOAuth(server.serverId, responseUri.trim());
    } catch (reason: unknown) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <section className="mcp-oauth">
      <div><KeyRound size={14} /><strong>需要 OAuth 授权</strong></div>
      {server.oauth.scopes.length === 0 ? null : <p>Scopes: {server.oauth.scopes.join(", ")}</p>}
      <a href={authorizationUrl} rel="noreferrer" target="_blank"><ExternalLink size={13} /> 打开授权页面</a>
      <label>
        授权后的完整回调 URL
        <input value={responseUri} type="url" onChange={(event) => setResponseUri(event.target.value)} />
      </label>
      <button disabled={submitting || responseUri.trim().length === 0} type="button" onClick={() => void continueAuthorization()}>
        {submitting ? "Authorizing…" : "Continue authorization"}
      </button>
      {error === undefined ? null : <div className="mcp-card__error">{error}</div>}
    </section>
  );
}
