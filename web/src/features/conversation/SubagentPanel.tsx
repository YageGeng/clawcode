import { useEffect, useState } from "react";
import type { SessionId } from "../../acp/protocol";
import type { WorkspaceController } from "../../workspace/controller";
import { Conversation } from "./Conversation";

/** Attaches a child's live transcript while leaving the owner's selection and URL intact. */
export function SubagentPanel({ sessionId, controller }: Readonly<{ sessionId: SessionId; controller: WorkspaceController }>) {
  const [error, setError] = useState<string>();
  useEffect(() => {
    let mounted = true;
    void controller.attachSession(sessionId).catch((reason: unknown) => {
      if (mounted) setError(reason instanceof Error ? reason.message : String(reason));
    });
    return () => { mounted = false; };
  }, [controller, sessionId]);
  return <section className="subagent-panel" aria-label="子会话">
    <header><strong>子会话</strong><button type="button" onClick={() => {
      void controller.openSession(sessionId).catch((reason: unknown) => setError(reason instanceof Error ? reason.message : String(reason)));
    }}>打开会话</button></header>
    {error === undefined ? null : <p role="alert">{error}</p>}
    <Conversation bootstrap={controller.bootstrap} controller={controller} sessionId={sessionId} compact onInspect={() => {}} />
  </section>;
}
